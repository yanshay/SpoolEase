use core::{cell::RefCell, str::Utf8Error};

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use embedded_hal_async::spi::SpiDevice;
use hashbrown::HashMap;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use snafu::prelude::*;

use framework::{
    info,
    prelude::{SDCardStore, SDCardStoreErrorSource},
    sdcard_store::Error as SDCardStoreError,
    warn,
};

use crate::utils::sha256_hex;

#[derive(Snafu)]
pub enum CsvDbError {
    #[snafu(display("SDCard File Operation Error {source:?}"))]
    Store { source: SDCardStoreErrorSource },

    #[snafu(display("Failed to parse database metadata : {source}"))]
    Metadata { source: serde_json::error::Error },

    #[snafu(display("Failed to deserialize record \"{record}\" : {source}"))]
    Deserialize { record: String, source: serde_csv_core::de::Error },

    #[snafu(display("Failed to serialize record: {source}"))]
    Serialize { source: serde_csv_core::ser::Error },

    #[snafu(display("Failed to UTF8 decode database : {source}"))]
    Utf8 { source: Utf8Error },

    #[snafu(display(" Internal Error : {details}"))]
    Internal { details: String },
}

impl core::fmt::Debug for CsvDbError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self)
    }
}

fn store_error_is_not_found(error: &SDCardStoreErrorSource) -> bool {
    match error {
        SDCardStoreError::Open { source, .. } | SDCardStoreError::ChangeDir { source, .. } | SDCardStoreError::Delete { source, .. } => {
            matches!(&source.0, embedded_sdmmc::asynchronous::Error::NotFound)
        }
        _ => false,
    }
}

pub trait CsvDbId {
    fn id(&self) -> &String;
}

#[derive(Debug)]
pub struct CsvRecordInfo<T>
where
    T: CsvDbId + Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    pub data: T,
    pub length_in_file: usize, // owned byte span in the DB file, including EOL
    offset: u32,
}

#[derive(Serialize, Deserialize)]
pub struct DbMetaFile {
    pub version: String,
}

pub struct CsvDbInner {
    db_file_name: String,
    dbm_file_name: String,
    max_record_width: usize,
    pub db_meta: DbMetaFile,
}

#[derive(Clone, Copy, Debug)]
pub struct CsvDbCompactionConfig {
    pub min_file_size_bytes: usize,
    pub max_waste_percent: usize,
    pub max_waste_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct CsvDbCompactionStats {
    file_size_bytes: usize,
    live_bytes: usize,
    free_bytes: usize,
    stale_bytes: usize,
}

impl CsvDbCompactionStats {
    fn waste_bytes(self) -> usize {
        self.free_bytes.saturating_add(self.stale_bytes)
    }

    fn waste_percent(self) -> usize {
        if self.file_size_bytes == 0 {
            0
        } else {
            self.waste_bytes().saturating_mul(100) / self.file_size_bytes
        }
    }

    fn should_compact(self, config: CsvDbCompactionConfig) -> bool {
        self.file_size_bytes >= config.min_file_size_bytes
            && (self.waste_percent() >= config.max_waste_percent || self.waste_bytes() >= config.max_waste_bytes)
    }
}

#[derive(Clone, Copy, Debug)]
struct FreeRange {
    offset: u32,
    length: usize,
}

struct ParsedCsvDb<T>
where
    T: CsvDbId + Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    records: HashMap<String, CsvRecordInfo<T>>,
    free_ranges: Vec<FreeRange>,
    stale_ranges: Vec<FreeRange>,
    stats: CsvDbCompactionStats,
}

#[derive(Clone, Debug)]
pub struct CsvDbCompactionFailure {
    pub details: String,
}

enum CsvDbCompactionReplaceResult<T>
where
    T: CsvDbId + Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    Completed {
        parsed: ParsedCsvDb<T>,
        failure: Option<CsvDbCompactionFailure>,
    },
    Skipped(CsvDbCompactionFailure),
}

pub struct CsvDb<T, SPI: SpiDevice, const MAX_DIRS: usize, const MAX_FILES: usize>
where
    T: CsvDbId + Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    sdcard: Rc<Mutex<CriticalSectionRawMutex, SDCardStore<SPI, MAX_DIRS, MAX_FILES>>>,
    pub inner: RefCell<CsvDbInner>,
    pub records: Rc<RefCell<HashMap<String, CsvRecordInfo<T>>>>,
    // Rebuilt on start. These ranges already contain dashed/empty bytes and are safe to overwrite.
    free_ranges: RefCell<Vec<FreeRange>>,
    operation_lock: Mutex<CriticalSectionRawMutex, ()>,
}

impl<T, SPI: SpiDevice, const MAX_DIRS: usize, const MAX_FILES: usize> CsvDb<T, SPI, MAX_DIRS, MAX_FILES>
where
    T: CsvDbId + Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    pub async fn new(
        sdcard: Rc<Mutex<CriticalSectionRawMutex, SDCardStore<SPI, MAX_DIRS, MAX_FILES>>>,
        db_name: &str,
        max_record_width: usize,
        min_capacity: usize,
        ver_if_new: &str,
    ) -> Result<Self, CsvDbError> {
        let dbm_file_name = format!("{db_name}.dbm");
        let db_file_name = format!("{db_name}.db");
        let sdcard_input = sdcard.clone();
        let records = HashMap::<String, CsvRecordInfo<T>>::with_capacity(min_capacity);

        let mut sdcard = sdcard.lock().await;
        let dbm_str = match sdcard.read_file_str(&dbm_file_name).await {
            Ok(dbm_str) if !dbm_str.is_empty() => dbm_str,
            Ok(_) => Self::initialize_new_database_files(&mut sdcard, &dbm_file_name, &db_file_name, ver_if_new).await?,
            Err(err) if store_error_is_not_found(&err) => {
                Self::initialize_new_database_files(&mut sdcard, &dbm_file_name, &db_file_name, ver_if_new).await?
            }
            Err(err) => return Err(CsvDbError::Store { source: err }),
        };
        let db_meta: DbMetaFile = serde_json::from_str(&dbm_str).context(MetadataSnafu)?;

        Ok(Self {
            inner: RefCell::new(CsvDbInner {
                db_file_name,
                dbm_file_name,
                max_record_width,
                db_meta,
            }),
            sdcard: sdcard_input.clone(),
            records: Rc::new(RefCell::new(records)),
            free_ranges: RefCell::new(Vec::with_capacity(min_capacity)),
            operation_lock: Mutex::new(()),
        })
    }

    async fn initialize_new_database_files(
        sdcard: &mut SDCardStore<SPI, MAX_DIRS, MAX_FILES>,
        dbm_file_name: &str,
        db_file_name: &str,
        ver_if_new: &str,
    ) -> Result<String, CsvDbError> {
        if Self::db_file_has_data(sdcard, db_file_name).await? {
            return Err(CsvDbError::Internal {
                details: format!("Database metadata file {dbm_file_name} is missing or empty, but {db_file_name} contains data"),
            });
        }

        let dbm = DbMetaFile {
            version: ver_if_new.to_string(),
        };
        let dbm_str = serde_json::to_string(&dbm).unwrap();
        sdcard.create_write_file_str(dbm_file_name, &dbm_str).await.context(StoreSnafu)?;
        sdcard.create_file(db_file_name).await.context(StoreSnafu)?;
        Ok(dbm_str)
    }

    async fn db_file_has_data(sdcard: &mut SDCardStore<SPI, MAX_DIRS, MAX_FILES>, db_file_name: &str) -> Result<bool, CsvDbError> {
        match sdcard.read_file_bytes(db_file_name).await {
            Ok(db_bytes) => Ok(!db_bytes.is_empty()),
            Err(err) if store_error_is_not_found(&err) => Ok(false),
            Err(err) => Err(CsvDbError::Store { source: err }),
        }
    }

    pub fn record_to_csv_string(&self, record: &CsvRecordInfo<T>) -> Result<String, CsvDbError> {
        let mut writer = serde_csv_core::Writer::new();
        let mut buffer = alloc::vec![0; self.inner.borrow().max_record_width];
        let length_written = writer.serialize(&record.data, buffer.as_mut_slice()).context(SerializeSnafu)?;
        buffer.truncate(length_written);
        // TODO: add this error as a source to the SerializeSnafu (so one error from several underlying sources)
        // Not critical since data will always be utf8
        let buffer_str = String::from_utf8(buffer).unwrap();
        Ok(buffer_str)
    }

    pub fn records_csv_len(&self) -> Result<usize, CsvDbError> {
        let mut writer = serde_csv_core::Writer::new();
        let mut buffer = alloc::vec![0; self.inner.borrow().max_record_width];
        let records = self.records.borrow();
        let mut total_length = 0_usize;
        for record in records.values() {
            let length_written = writer.serialize(&record.data, buffer.as_mut_slice()).context(SerializeSnafu)?;
            total_length = total_length.checked_add(length_written).ok_or_else(|| CsvDbError::Internal {
                details: "CSV length overflow".to_string(),
            })?;
        }
        Ok(total_length)
    }

    pub fn write_records_csv(&self, output: &mut [u8]) -> Result<usize, CsvDbError> {
        let mut writer = serde_csv_core::Writer::new();
        let records = self.records.borrow();
        let mut pos = 0_usize;
        for record in records.values() {
            let length_written = writer.serialize(&record.data, &mut output[pos..]).context(SerializeSnafu)?;
            pos = pos.checked_add(length_written).ok_or_else(|| CsvDbError::Internal {
                details: "CSV length overflow".to_string(),
            })?;
        }
        Ok(pos)
    }

    fn free_range_end(range: FreeRange) -> u32 {
        range.offset + range.length as u32
    }

    fn add_free_range(free_ranges: &mut Vec<FreeRange>, offset: u32, length: usize) {
        if length == 0 {
            return;
        }

        let mut new_offset = offset;
        let mut new_end = offset + length as u32;
        let mut index = 0;

        while index < free_ranges.len() {
            let range = free_ranges[index];
            let range_end = Self::free_range_end(range);

            if range_end < new_offset {
                index += 1;
                continue;
            }

            if new_end < range.offset {
                break;
            }

            // Overlapping or directly adjacent free ranges are one reusable space.
            new_offset = new_offset.min(range.offset);
            new_end = new_end.max(range_end);
            free_ranges.remove(index);
        }

        free_ranges.insert(
            index,
            FreeRange {
                offset: new_offset,
                length: (new_end - new_offset) as usize,
            },
        );
    }

    fn find_free_range(free_ranges: &[FreeRange], length: usize, min_offset: Option<u32>) -> Option<usize> {
        // First-fit is enough for the small database sizes this file handles.
        free_ranges.iter().position(|range| {
            range.length >= length
                && match min_offset {
                    Some(min_offset) => range.offset >= min_offset,
                    None => true,
                }
        })
    }

    fn consume_free_range(free_ranges: &mut Vec<FreeRange>, index: usize, used_length: usize) {
        if used_length >= free_ranges[index].length {
            free_ranges.remove(index);
        } else {
            free_ranges[index].offset += used_length as u32;
            free_ranges[index].length -= used_length;
        }
    }

    fn empty_record_buffer(length: usize) -> Vec<u8> {
        let mut buffer = alloc::vec![b'-'; length];
        if length > 0 {
            buffer[length - 1] = b'\n';
        }
        buffer
    }

    async fn write_empty_range(
        sdcard: &mut SDCardStore<SPI, MAX_DIRS, MAX_FILES>,
        db_file_name: &str,
        offset: u32,
        length: usize,
    ) -> Result<(), CsvDbError> {
        let empty_buffer = Self::empty_record_buffer(length);
        sdcard
            .write_file_bytes(db_file_name, offset, empty_buffer.as_slice(), false)
            .await
            .context(StoreSnafu)?;
        Ok(())
    }

    fn parse_db_str(_db_filename: &str, db_str: &str, min_capacity: usize) -> Result<ParsedCsvDb<T>, CsvDbError> {
        let mut reader = serde_csv_core::Reader::<256>::new(); // 100 is max field size
        let mut nread = 0;
        let mut free_bytes = 0;
        let mut stale_bytes = 0;
        let mut records = HashMap::<String, CsvRecordInfo<T>>::with_capacity(min_capacity);
        let mut free_ranges = Vec::with_capacity(min_capacity);
        let mut stale_ranges = Vec::<FreeRange>::new();
        for line in db_str.lines() {
            let line_length = line.len() + 1;
            if line.is_empty() || line.chars().all(|c| c == '-') {
                free_bytes += line_length;
                Self::add_free_range(&mut free_ranges, nread as u32, line_length);
            } else {
                let (record, _record_length) = reader.deserialize::<T>(line.as_bytes()).context(DeserializeSnafu { record: line })?;
                let record_info = CsvRecordInfo {
                    data: record,
                    offset: nread as u32,
                    length_in_file: line_length,
                };
                let record_id = record_info.data.id().clone();
                if let Some(previous_record_info) = records.insert(record_id, record_info) {
                    // Later duplicates win. The older visible row is cleaned after the file is fully loaded.
                    stale_bytes += previous_record_info.length_in_file;
                    stale_ranges.push(FreeRange {
                        offset: previous_record_info.offset,
                        length: previous_record_info.length_in_file,
                    });
                }
            }
            nread += line_length;
        }

        let live_bytes = records.values().map(|record| record.length_in_file).sum();
        Ok(ParsedCsvDb {
            records,
            free_ranges,
            stale_ranges,
            stats: CsvDbCompactionStats {
                file_size_bytes: db_str.len(),
                live_bytes,
                free_bytes,
                stale_bytes,
            },
        })
    }

    fn build_compacted_file(records: &HashMap<String, CsvRecordInfo<T>>, max_record_width: usize) -> Result<Vec<u8>, CsvDbError> {
        let mut record_buffer = alloc::vec![0u8; max_record_width];
        let mut writer = serde_csv_core::Writer::new();
        let mut length_required = 0;
        for record in records.values() {
            let serialized_len = writer.serialize(&record.data, record_buffer.as_mut_slice()).context(SerializeSnafu)?;
            length_required += serialized_len;
        }

        let mut file_buffer = alloc::vec![0u8; length_required];
        let mut pos = 0;
        for record in records.values() {
            let length_written = writer.serialize(&record.data, &mut file_buffer[pos..]).context(SerializeSnafu)?;
            pos += length_written;
        }

        Ok(file_buffer)
    }

    fn split_parent_child(path: &str) -> Result<(String, String), CsvDbError> {
        let trimmed = path.trim_end_matches('/');
        let Some(index) = trimmed.rfind('/') else {
            return Err(CsvDbError::Internal {
                details: format!("Path '{path}' has no parent directory"),
            });
        };
        let child = &trimmed[index + 1..];
        if child.is_empty() {
            return Err(CsvDbError::Internal {
                details: format!("Path '{path}' has no file name"),
            });
        }

        let parent = if index == 0 { "/" } else { &trimmed[..index] };
        Ok((parent.to_string(), child.to_string()))
    }

    fn db_artifact_filename(db_filename: &str, extension: &str) -> Result<String, CsvDbError> {
        let db_filename_prefix = db_filename.strip_suffix(".db").ok_or_else(|| CsvDbError::Internal {
            details: "DB filename doesn't end with '.db'".to_string(),
        })?;
        Ok(format!("{db_filename_prefix}.{extension}"))
    }

    async fn delete_file_if_exists(sdcard: &mut SDCardStore<SPI, MAX_DIRS, MAX_FILES>, path: &str) -> Result<(), CsvDbError> {
        match sdcard.delete_file(path).await {
            Ok(()) => Ok(()),
            Err(err) if store_error_is_not_found(&err) => Ok(()),
            Err(err) => Err(CsvDbError::Store { source: err }),
        }
    }

    async fn recover_missing_db_from_old(sdcard: &mut SDCardStore<SPI, MAX_DIRS, MAX_FILES>, db_filename: &str) -> Result<(), CsvDbError> {
        if sdcard.file_exists(db_filename).await.context(StoreSnafu)? {
            return Ok(());
        }

        let old_filename = Self::db_artifact_filename(db_filename, "old")?;
        if !sdcard.file_exists(&old_filename).await.context(StoreSnafu)? {
            return Ok(());
        }

        let (parent, db_child) = Self::split_parent_child(db_filename)?;
        let (_, old_child) = Self::split_parent_child(&old_filename)?;
        warn!("Recovering missing {db_filename} from {old_filename}");
        sdcard.rename_entry_in_dir(&parent, &old_child, &db_child).await.context(StoreSnafu)
    }

    async fn cleanup_compaction_artifacts(sdcard: &mut SDCardStore<SPI, MAX_DIRS, MAX_FILES>, db_filename: &str) {
        let new_filename = Self::db_artifact_filename(db_filename, "new");
        let old_filename = Self::db_artifact_filename(db_filename, "old");
        for path in [new_filename, old_filename] {
            let Ok(path) = path else {
                continue;
            };
            if let Err(err) = Self::delete_file_if_exists(sdcard, &path).await {
                warn!("Failed cleaning up compaction artifact {path}: {err:?}");
            }
        }
    }

    async fn safe_replace_with_compacted_file(
        sdcard: &mut SDCardStore<SPI, MAX_DIRS, MAX_FILES>,
        db_filename: &str,
        compacted_file: Vec<u8>,
        expected_records: usize,
        min_capacity: usize,
    ) -> Result<CsvDbCompactionReplaceResult<T>, CsvDbError> {
        let new_filename = Self::db_artifact_filename(db_filename, "new")?;
        let old_filename = Self::db_artifact_filename(db_filename, "old")?;
        let (parent, db_child) = Self::split_parent_child(db_filename)?;
        let (_, new_child) = Self::split_parent_child(&new_filename)?;
        let (_, old_child) = Self::split_parent_child(&old_filename)?;
        let expected_sha = sha256_hex(&compacted_file);

        if let Err(err) = sdcard.create_write_file_bytes(&new_filename, &compacted_file).await {
            let details = format!("failed writing {new_filename}: {err:?}");
            warn!("Compaction skipped: {details}");
            return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
        }
        drop(compacted_file);

        let new_file_bytes = match sdcard.read_file_bytes(&new_filename).await {
            Ok(bytes) => bytes,
            Err(err) => {
                let details = format!("failed reading {new_filename}: {err:?}");
                warn!("Compaction skipped: {details}");
                let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
                return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
            }
        };
        let actual_sha = sha256_hex(&new_file_bytes);
        if actual_sha != expected_sha {
            let details = format!("verification hash mismatch for {new_filename}: expected {expected_sha}, got {actual_sha}");
            warn!("Compaction skipped: {details}");
            let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
            return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
        }
        let parsed_new_file = {
            let new_file_str = match core::str::from_utf8(&new_file_bytes) {
                Ok(str) => str,
                Err(err) => {
                    let details = format!("{new_filename} is not valid UTF-8: {err}");
                    warn!("Compaction skipped: {details}");
                    let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
                    return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
                }
            };
            match Self::parse_db_str(&new_filename, new_file_str, min_capacity) {
                Ok(parsed) if parsed.records.len() == expected_records => parsed,
                Ok(parsed) => {
                    let details = format!("{new_filename} has {} records, expected {}", parsed.records.len(), expected_records);
                    warn!("Compaction skipped: {details}");
                    let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
                    return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
                }
                Err(err) => {
                    let details = format!("{new_filename} failed validation: {err:?}");
                    warn!("Compaction skipped: {details}");
                    let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
                    return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
                }
            }
        };
        drop(new_file_bytes);

        if let Err(err) = Self::delete_file_if_exists(sdcard, &old_filename).await {
            let details = format!("failed clearing old backup {old_filename}: {err:?}");
            warn!("Compaction skipped: {details}");
            let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
            return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
        }

        if let Err(err) = sdcard.rename_entry_in_dir(&parent, &db_child, &old_child).await {
            let details = format!("failed renaming {db_filename} to {old_filename}: {err:?}");
            warn!("Compaction skipped: {details}");
            let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
            return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
        }

        if let Err(err) = sdcard.rename_entry_in_dir(&parent, &new_child, &db_child).await {
            let details = format!("failed renaming {new_filename} to {db_filename}: {err:?}");
            warn!("Compaction failed: {details}");
            match sdcard.rename_entry_in_dir(&parent, &old_child, &db_child).await {
                Ok(()) => {
                    let _ = Self::delete_file_if_exists(sdcard, &new_filename).await;
                    return Ok(CsvDbCompactionReplaceResult::Skipped(CsvDbCompactionFailure { details }));
                }
                Err(rollback_err) => {
                    return Err(CsvDbError::Internal {
                        details: format!("Compaction left {db_filename} unavailable; rollback from {old_filename} failed: {rollback_err:?}"),
                    });
                }
            }
        }

        let cleanup_failure = if let Err(err) = Self::delete_file_if_exists(sdcard, &old_filename).await {
            let details = format!("compaction completed, but failed deleting {old_filename}: {err:?}");
            warn!("{details}");
            Some(CsvDbCompactionFailure { details })
        } else {
            None
        };

        Ok(CsvDbCompactionReplaceResult::Completed {
            parsed: parsed_new_file,
            failure: cleanup_failure,
        })
    }

    pub async fn start(&mut self, backup: bool, pack: bool) -> Result<(), CsvDbError> {
        self.start_inner(backup, pack, None).await.map(|_| ())
    }

    pub async fn start_with_compaction(
        &mut self,
        backup: bool,
        compaction_config: CsvDbCompactionConfig,
    ) -> Result<Option<CsvDbCompactionFailure>, CsvDbError> {
        self.start_inner(backup, false, Some(compaction_config)).await
    }

    async fn start_inner(
        &mut self,
        backup: bool,
        force_compaction: bool,
        compaction_config: Option<CsvDbCompactionConfig>,
    ) -> Result<Option<CsvDbCompactionFailure>, CsvDbError> {
        // Now read db file

        let mut sdcard = self.sdcard.lock().await;
        let db_filename = self.inner.borrow().db_file_name.clone();
        let records_capacity = self.records.borrow().capacity();
        Self::recover_missing_db_from_old(&mut sdcard, &db_filename).await?;
        let db_bytes = match sdcard.read_file_bytes(&db_filename).await {
            Ok(db_bytes) => db_bytes,
            Err(err) if store_error_is_not_found(&err) => Vec::new(),
            Err(err) => return Err(CsvDbError::Store { source: err }),
        };
        let mut parsed = {
            let db_str = core::str::from_utf8(&db_bytes).context(Utf8Snafu)?;
            let parsed = Self::parse_db_str(&db_filename, db_str, records_capacity)?;

            // Don't copy in case records are empty (so not destroy old backup)
            if backup && !parsed.records.is_empty() {
                let backup_file_name = Self::db_artifact_filename(&db_filename, "db1")?;
                sdcard.create_write_file_str(&backup_file_name, db_str).await.context(StoreSnafu)?;
                Self::cleanup_compaction_artifacts(&mut sdcard, &db_filename).await;
            }

            parsed
        };
        drop(db_bytes);

        let should_compact =
            !parsed.records.is_empty() && (force_compaction || compaction_config.map(|config| parsed.stats.should_compact(config)).unwrap_or(false));
        let mut compaction_completed = false;
        let mut compaction_failure = None;
        if should_compact {
            info!(
                "Compacting {}: size={} live={} waste={} waste_percent={} free={} stale={}",
                db_filename,
                parsed.stats.file_size_bytes,
                parsed.stats.live_bytes,
                parsed.stats.waste_bytes(),
                parsed.stats.waste_percent(),
                parsed.stats.free_bytes,
                parsed.stats.stale_bytes
            );
            let max_record_width = self.inner.borrow().max_record_width;
            match Self::build_compacted_file(&parsed.records, max_record_width) {
                Ok(compacted_file) => {
                    let compacted_file_len = compacted_file.len();
                    match Self::safe_replace_with_compacted_file(&mut sdcard, &db_filename, compacted_file, parsed.records.len(), records_capacity)
                        .await?
                    {
                        CsvDbCompactionReplaceResult::Completed {
                            parsed: compacted_db,
                            failure,
                        } => {
                            info!(
                                "Compacted {} from {} to {} bytes",
                                db_filename, parsed.stats.file_size_bytes, compacted_file_len
                            );
                            parsed = compacted_db;
                            compaction_completed = true;
                            if failure.is_some() {
                                compaction_failure = failure;
                            }
                        }
                        CsvDbCompactionReplaceResult::Skipped(failure) => {
                            compaction_failure = Some(failure);
                        }
                    }
                }
                Err(err) => {
                    let details = format!("failed building compacted content for {db_filename}: {err:?}");
                    warn!("Compaction skipped: {details}");
                    compaction_failure = Some(CsvDbCompactionFailure { details });
                }
            }
        }

        if !compaction_completed {
            // Packing removes stale duplicates by rewriting active records only. Without packing,
            // dash older duplicates now so users do not see obsolete rows in the CSV file.
            for stale_range in parsed.stale_ranges.iter() {
                if Self::write_empty_range(&mut sdcard, &db_filename, stale_range.offset, stale_range.length)
                    .await
                    .is_ok()
                {
                    Self::add_free_range(&mut parsed.free_ranges, stale_range.offset, stale_range.length);
                }
            }
        }

        *self.records.borrow_mut() = parsed.records;
        *self.free_ranges.borrow_mut() = parsed.free_ranges;

        Ok(compaction_failure)
    }

    pub async fn save_all_records_only_before_use(&self) -> Result<(), CsvDbError> {
        let _operation_guard = self.operation_lock.lock().await;
        let max_record_width = self.inner.borrow().max_record_width;
        let records_capacity = self.records.borrow().capacity();
        let (file_buffer, expected_records) = {
            let records = self.records.borrow();
            (Self::build_compacted_file(&records, max_record_width)?, records.len())
        };

        let db_filename = self.inner.borrow().db_file_name.clone();
        let mut sdcard = self.sdcard.lock().await;
        let parsed = match Self::safe_replace_with_compacted_file(&mut sdcard, &db_filename, file_buffer, expected_records, records_capacity).await? {
            CsvDbCompactionReplaceResult::Completed { parsed, .. } => parsed,
            CsvDbCompactionReplaceResult::Skipped(failure) => {
                return Err(CsvDbError::Internal {
                    details: format!("Safe rewrite of {db_filename} did not complete: {}", failure.details),
                });
            }
        };

        *self.records.borrow_mut() = parsed.records;
        *self.free_ranges.borrow_mut() = parsed.free_ranges;
        Ok(())
    }
    pub async fn update_version(&self, version: &str) -> Result<(), CsvDbError> {
        self.inner.borrow_mut().db_meta.version = version.to_string();
        let dbm_file_name = self.inner.borrow().dbm_file_name.clone();
        let dbm_str = serde_json::to_string(&self.inner.borrow().db_meta).unwrap();
        let mut sdcard = self.sdcard.lock().await;
        sdcard.create_write_file_str(&dbm_file_name, &dbm_str).await.context(StoreSnafu)?;
        Ok(())
    }

    pub async fn insert(&self, record: T) -> Result<bool, CsvDbError> {
        let _operation_guard = self.operation_lock.lock().await;
        let (already_exist, prev_offset, prev_length) = if let Some(v) = self.records.borrow().get(record.id()) {
            if v.data == record {
                return Ok(false);
            }
            (true, v.offset, v.length_in_file)
        } else {
            (false, 0, 0)
        };
        let mut buffer = alloc::vec![0;self.inner.borrow().max_record_width];
        let serialized_len = self.calc_csv_row(&record, &mut buffer)?;
        let db_file_name = self.inner.borrow().db_file_name.clone();
        let final_offset;
        let mut sdcard = self.sdcard.lock().await;

        if already_exist && serialized_len <= prev_length {
            final_offset = prev_offset;
            if serialized_len == prev_length {
                sdcard
                    .write_file_bytes(&db_file_name, prev_offset, &buffer[..serialized_len], false)
                    .await
                    .context(StoreSnafu)?;
            } else {
                let remaining_len = prev_length - serialized_len;
                let mut overwrite_buffer = Vec::with_capacity(prev_length);
                overwrite_buffer.extend_from_slice(&buffer[..serialized_len]);
                overwrite_buffer.extend_from_slice(&Self::empty_record_buffer(remaining_len));
                sdcard
                    .write_file_bytes(&db_file_name, prev_offset, &overwrite_buffer, false)
                    .await
                    .context(StoreSnafu)?;
                Self::add_free_range(&mut self.free_ranges.borrow_mut(), prev_offset + serialized_len as u32, remaining_len);
            }
        } else {
            let free_range = {
                let free_ranges = self.free_ranges.borrow();
                Self::find_free_range(&free_ranges, serialized_len, None).map(|index| (index, free_ranges[index].offset))
            };

            if let Some((free_range_index, free_range_offset)) = free_range {
                // Once we try writing into a free range, it is no longer known-clean if the write fails.
                Self::consume_free_range(&mut self.free_ranges.borrow_mut(), free_range_index, serialized_len);
                sdcard
                    .write_file_bytes(&db_file_name, free_range_offset, &buffer[..serialized_len], false)
                    .await
                    .context(StoreSnafu)?;
                final_offset = free_range_offset;
            } else {
                final_offset = sdcard.append_bytes(&db_file_name, &buffer[..serialized_len]).await.context(StoreSnafu)?;
            }

            if already_exist {
                Self::write_empty_range(&mut sdcard, &db_file_name, prev_offset, prev_length).await?;
                Self::add_free_range(&mut self.free_ranges.borrow_mut(), prev_offset, prev_length);
            }
        }

        {
            let mut records_borrow = self.records.borrow_mut();

            if let Some(v) = records_borrow.get_mut(record.id()) {
                v.data = record;
                v.offset = final_offset;
                v.length_in_file = serialized_len;
            } else {
                let csv_record_info = CsvRecordInfo {
                    data: record,
                    offset: final_offset,
                    length_in_file: serialized_len,
                };
                records_borrow.insert(csv_record_info.data.id().clone(), csv_record_info);
            }
        }

        Ok(!already_exist)
    }

    #[allow(dead_code)]
    pub async fn delete(&self, id: &str) -> Result<Option<T>, CsvDbError> {
        let _operation_guard = self.operation_lock.lock().await;
        let (offset, length) = if let Some(v) = self.records.borrow().get(id) {
            (v.offset, v.length_in_file)
        } else {
            return Ok(None);
        };

        let mut sdcard = self.sdcard.lock().await;
        let db_file_name = self.inner.borrow().db_file_name.clone();
        Self::write_empty_range(&mut sdcard, &db_file_name, offset, length).await?;

        if let Some(record) = self.records.borrow_mut().remove(id) {
            Self::add_free_range(&mut self.free_ranges.borrow_mut(), offset, length);
            return Ok(Some(record.data));
        }
        Ok(None)
    }

    fn inner_calc_csv_row(record: &T, buffer: &mut Vec<u8>) -> Result<usize, CsvDbError> {
        let mut writer = serde_csv_core::Writer::new();
        let length_written = writer.serialize(record, buffer.as_mut_slice()).context(SerializeSnafu)?;
        Ok(length_written)
    }

    fn calc_csv_row(&self, record: &T, buffer: &mut Vec<u8>) -> Result<usize, CsvDbError> {
        buffer.resize(self.inner.borrow().max_record_width, 0);
        Self::inner_calc_csv_row(record, buffer)
    }
}
