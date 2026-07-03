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
    prelude::{SDCardStore, SDCardStoreErrorSource},
    sdcard_store::Error as SDCardStoreError,
};

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
        SDCardStoreError::Open { source, .. } | SDCardStoreError::ChangeDir { source, .. } => {
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
struct FreeRange {
    offset: u32,
    length: usize,
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

    // TODO: packing is risky under certain sdcard errors since it does truncate first.
    // a proper packing should create dbname.new, then rename name.db to name.old, then rename dbname.new to dbname.db and then remove dbname.old
    // and deal with potential failures during and inbetween these operations
    pub async fn start(&mut self, backup: bool, pack: bool) -> Result<(), CsvDbError> {
        // Now read db file

        let mut sdcard = self.sdcard.lock().await;
        let db_filename = self.inner.borrow().db_file_name.clone();
        let db_bytes = match sdcard.read_file_bytes(&db_filename).await {
            Ok(db_bytes) => db_bytes,
            Err(err) if store_error_is_not_found(&err) => Vec::new(),
            Err(err) => return Err(CsvDbError::Store { source: err }),
        };
        let db_str = core::str::from_utf8(&db_bytes).context(Utf8Snafu)?;
        let mut reader = serde_csv_core::Reader::<256>::new(); // 100 is max field size
        let mut nread = 0;
        let mut _data_nread = 0;
        let mut _empty_nread = 0;
        let mut records = self.records.take();
        records.clear();
        let mut free_ranges = self.free_ranges.take();
        free_ranges.clear();
        let mut stale_ranges = Vec::<FreeRange>::new();
        for line in db_str.lines() {
            let line_length = line.len() + 1;
            if line.is_empty() || line.chars().all(|c| c == '-') {
                _empty_nread += line_length;
                Self::add_free_range(&mut free_ranges, nread as u32, line_length);
            } else {
                _data_nread += line_length;
                let (record, _record_length) = reader.deserialize::<T>(line.as_bytes()).context(DeserializeSnafu { record: line })?;
                let record_info = CsvRecordInfo {
                    data: record,
                    offset: nread as u32,
                    length_in_file: line_length,
                };
                let record_id = record_info.data.id().clone();
                if let Some(previous_record_info) = records.insert(record_id, record_info) {
                    // Later duplicates win. The older visible row is cleaned after the file is fully loaded.
                    stale_ranges.push(FreeRange {
                        offset: previous_record_info.offset,
                        length: previous_record_info.length_in_file,
                    });
                }
            }
            nread += line_length;
        }

        // Don't copy in case records are empty (so not destroy old backup)
        if backup && !records.is_empty() {
            let db_filename_prefix = db_filename.strip_suffix(".db").ok_or_else(|| CsvDbError::Internal {
                details: "DB filename doesn't end with '.db.'".to_string(),
            })?;
            let backup_file_name = format!("{}.db1", db_filename_prefix);
            sdcard.create_write_file_str(&backup_file_name, db_str).await.context(StoreSnafu)?;
        }

        // Now pack if requested
        // Check items size and not use current size in case of type change and serialize longer than original read data
        // Don't pack if empty (in case of some error missed when reading file and no records read)
        if pack && !records.is_empty() {
            // TODO: use the save_all_records_only_before_use instead this code after see it is ok
            let mut record_buffer = alloc::vec![0u8;self.inner.borrow().max_record_width];
            let mut writer = serde_csv_core::Writer::new();
            let mut length_required = 0;
            for record in records.iter() {
                let serialized_len = writer.serialize(&record.1.data, record_buffer.as_mut_slice()).context(SerializeSnafu)?;
                length_required += serialized_len;
            }
            let mut file_buffer = alloc::vec![b'-'; length_required];
            let mut pos = 0;
            for record in records.iter_mut() {
                let length_written = writer.serialize(&record.1.data, &mut file_buffer[pos..]).context(SerializeSnafu)?;
                record.1.offset = pos as u32;
                record.1.length_in_file = length_written;
                pos += length_written;
            }
            sdcard.create_write_file_bytes(&db_filename, &file_buffer).await.context(StoreSnafu)?;
            free_ranges.clear();
        } else {
            // Packing removes stale duplicates by rewriting active records only. Without packing,
            // dash older duplicates now so users do not see obsolete rows in the CSV file.
            for stale_range in stale_ranges {
                if Self::write_empty_range(&mut sdcard, &db_filename, stale_range.offset, stale_range.length)
                    .await
                    .is_ok()
                {
                    Self::add_free_range(&mut free_ranges, stale_range.offset, stale_range.length);
                }
            }
        }

        *self.records.borrow_mut() = records;
        *self.free_ranges.borrow_mut() = free_ranges;

        Ok(())
    }

    pub async fn save_all_records_only_before_use(&self) -> Result<(), CsvDbError> {
        let (file_buffer, record_positions) = {
            let mut record_buffer = alloc::vec![0u8;self.inner.borrow().max_record_width];
            let mut writer = serde_csv_core::Writer::new();
            let mut length_required = 0;
            let records = self.records.borrow();
            for record in records.iter() {
                let serialized_len = writer.serialize(&record.1.data, record_buffer.as_mut_slice()).context(SerializeSnafu)?;
                length_required += serialized_len;
            }
            let mut file_buffer = alloc::vec![b'-'; length_required];
            let mut pos = 0;
            let mut record_positions = Vec::with_capacity(records.len());
            for record in records.iter() {
                let length_written = writer.serialize(&record.1.data, &mut file_buffer[pos..]).context(SerializeSnafu)?;
                record_positions.push((record.0.clone(), pos as u32, length_written));
                pos += length_written;
            }

            (file_buffer, record_positions)
        };

        let db_filename = self.inner.borrow().db_file_name.clone();
        let mut sdcard = self.sdcard.lock().await;
        sdcard.create_write_file_bytes(&db_filename, &file_buffer).await.context(StoreSnafu)?;

        let mut records = self.records.borrow_mut();
        for (id, offset, length) in record_positions {
            if let Some(record) = records.get_mut(&id) {
                record.offset = offset;
                record.length_in_file = length;
            }
        }
        self.free_ranges.borrow_mut().clear();
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
