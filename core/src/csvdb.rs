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
    pub length: usize, // including EOL (\n at this time, so +1)
    offset: u32,
}

impl<T> CsvRecordInfo<T>
where
    T: CsvDbId + Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    pub fn to_csv_string(&self) -> Result<String, CsvDbError> {
        let mut writer = serde_csv_core::Writer::new();
        let mut buffer = alloc::vec![0; self.length];
        let length_written = writer.serialize(&self.data, buffer.as_mut_slice()).context(SerializeSnafu)?;
        buffer.truncate(length_written);
        // TODO: add this error as a source to the SerializeSnafu (so one error from several underlying sources)
        // Not critical since data will always be utf8
        let buffer_str = String::from_utf8(buffer).unwrap();
        Ok(buffer_str)
    }
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

pub struct CsvDb<T, SPI: SpiDevice, const MAX_DIRS: usize, const MAX_FILES: usize>
where
    T: CsvDbId + Serialize + DeserializeOwned + PartialEq + core::fmt::Debug,
{
    sdcard: Rc<Mutex<CriticalSectionRawMutex, SDCardStore<SPI, MAX_DIRS, MAX_FILES>>>,
    pub inner: RefCell<CsvDbInner>,
    pub records: Rc<RefCell<HashMap<String, CsvRecordInfo<T>>>>,
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
        for line in db_str.lines() {
            if line.is_empty() {
                _empty_nread += 1;
            } else if line.chars().all(|c| c == '-') {
                _empty_nread += line.len() + 1;
            } else {
                _data_nread += line.len() + 1;
                let (record, record_length) = reader.deserialize::<T>(line.as_bytes()).context(DeserializeSnafu { record: line })?;
                let record_info = CsvRecordInfo {
                    data: record,
                    offset: nread as u32,
                    length: record_length + 1,
                };
                records.insert(record_info.data.id().clone(), record_info);
            }
            nread = nread + line.len() + 1;
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
                record.1.length = length_written;
                pos += length_written;
            }
            self.records = Rc::new(RefCell::new(records));
            sdcard.create_write_file_bytes(&db_filename, &file_buffer).await.context(StoreSnafu)?;
        }

        Ok(())
    }

    pub async fn save_all_records_only_before_use(&self) -> Result<(), CsvDbError> {
        let mut record_buffer = alloc::vec![0u8;self.inner.borrow().max_record_width];
        let mut writer = serde_csv_core::Writer::new();
        let mut length_required = 0;
        let mut records = self.records.take();
        for record in records.iter() {
            let serialized_len = writer.serialize(&record.1.data, record_buffer.as_mut_slice()).context(SerializeSnafu)?;
            length_required += serialized_len;
        }
        let mut file_buffer = alloc::vec![b'-'; length_required];
        let mut pos = 0;
        for record in records.iter_mut() {
            let length_written = writer.serialize(&record.1.data, &mut file_buffer[pos..]).context(SerializeSnafu)?;
            record.1.offset = pos as u32;
            record.1.length = length_written;
            pos += length_written;
        }
        *self.records.borrow_mut() = records;
        let db_filename = self.inner.borrow().db_file_name.clone();
        let mut sdcard = self.sdcard.lock().await;
        sdcard.create_write_file_bytes(&db_filename, &file_buffer).await.context(StoreSnafu)?;
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
            (true, v.offset, v.length)
        } else {
            (false, 0, 0)
        };
        let mut buffer = alloc::vec![0;self.inner.borrow().max_record_width];
        let serialized_len = self.calc_csv_row(&record, &mut buffer)?;
        let db_file_name = self.inner.borrow().db_file_name.clone();
        let mut final_offset;
        let mut sdcard = self.sdcard.lock().await;
        if already_exist {
            if serialized_len <= prev_length {
                buffer[serialized_len..prev_length].fill(b'-');
                buffer[prev_length - 1] = b'\n';
                final_offset = Some(prev_offset);
                sdcard
                    .write_file_bytes(&db_file_name, prev_offset, &buffer[..prev_length], false)
                    .await
                    .context(StoreSnafu)?;
            } else {
                let mut empty_buffer = alloc::vec![b'-';prev_length];
                empty_buffer[prev_length - 1] = b'\n';
                sdcard
                    .write_file_bytes(&db_file_name, prev_offset, empty_buffer.as_slice(), false)
                    .await
                    .context(StoreSnafu)?;
                final_offset = None;
            }
        } else {
            final_offset = None;
        }

        if final_offset.is_none() {
            final_offset = Some(sdcard.append_bytes(&db_file_name, &buffer[..serialized_len]).await.context(StoreSnafu)?);
        }

        let mut records_borrow = self.records.borrow_mut();

        if let Some(v) = records_borrow.get_mut(record.id()) {
            v.data = record;
            v.offset = final_offset.unwrap();
            v.length = serialized_len;
        } else {
            let csv_record_info = CsvRecordInfo {
                data: record,
                offset: final_offset.unwrap(),
                length: serialized_len,
            };
            records_borrow.insert(csv_record_info.data.id().clone(), csv_record_info);
        }

        Ok(!already_exist)
    }

    #[allow(dead_code)]
    pub async fn delete(&self, id: &str) -> Result<Option<T>, CsvDbError> {
        let (offset, length) = if let Some(v) = self.records.borrow().get(id) {
            (v.offset, v.length)
        } else {
            return Ok(None);
        };

        let mut buffer = Vec::<u8>::with_capacity(self.inner.borrow().max_record_width);
        self.calc_empty_record(&mut buffer, length);
        let mut sdcard = self.sdcard.lock().await;
        let db_file_name = self.inner.borrow().db_file_name.clone();
        sdcard
            .write_file_bytes(&db_file_name, offset, buffer.as_slice(), false)
            .await
            .context(StoreSnafu)?;

        if let Some(record) = self.records.borrow_mut().remove(id) {
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

    fn calc_empty_record(&self, buffer: &mut Vec<u8>, length: usize) {
        buffer.clear();
        buffer.resize(length, b'-');
        buffer[length - 1] = b'\n';
    }
}
