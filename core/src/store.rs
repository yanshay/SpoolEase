use core::cell::RefCell;
use embassy_time::{Instant, Timer};
use hashbrown::HashMap;
use once_cell::unsync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::Deserializer;
use snafu::prelude::*;

use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
// use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use framework::{
    debug, error, info,
    ntp::InstantExt,
    prelude::*,
    settings::{FILE_STORE_MAX_DIRS, FILE_STORE_MAX_FILES},
    term_error, term_info, warn,
};

use crate::{
    bambu::calibration::KInfo,
    csvdb::{CsvDb, CsvDbError},
    spools_storage::{StorageConfig, TagLocationRecord},
    tag_v1::TagInformationV1,
    view_model::ViewModel,
};

use crate::spool_record::{SpoolRecord, SpoolRecordExt};

const SPOOLS_STORE_VER: &str = "1.1.0";
const LOCATIONS_STORE_VER: &str = "1.0.0";

#[derive(Snafu, Debug)]
pub enum StoreError {
    #[snafu(display("Too many store operations pending"))]
    TooManyOps,

    #[snafu(display("CsvDbError : {source:?}"))]
    CsvDbError { source: CsvDbError },

    #[snafu(display("SDCard File Operation Error {source:?}"))]
    Store { source: SDCardStoreErrorSource },

    #[snafu(display("Internal store software logic error"))]
    InternalError,

    #[snafu(display("Record not found"))]
    NotFound { id: String },

    #[snafu(display("Can't access databse (SD Card Installed?)"))]
    NoCsvDb,

    #[snafu(display("Missing required id for operation in record"))]
    MissingId,

    #[snafu(display("Bad Id for operation"))]
    BadId,

    #[snafu(display("Id not found in databse"))]
    IdNotFound,

    #[snafu(display("Can't read extended file"))]
    ExtFileReadFailure { error: String },

    #[snafu(display("Extended record format error"))]
    ExtFormat { source: serde_json::error::Error },
}

// DON'T ERASE - May be useful in the future
// // Cookie - General code
// pub trait AnyClone: Any + core::fmt::Debug {
//     fn clone_box(&self) -> Box<dyn AnyClone>;
//     fn into_any(self: Box<Self>) -> Box<dyn Any>;
//     fn as_any(&self) -> &dyn Any;
// }
//
// pub trait Cookie: Any + Clone + core::fmt::Debug + 'static {}
//
// impl<T> AnyClone for T
// where
//     T: Cookie, // Any + Clone  + core::fmt::Debug + 'static,
// {
//     fn clone_box(&self) -> Box<dyn AnyClone> {
//         Box::new(self.clone())
//     }
//
//     fn into_any(self: Box<Self>) -> Box<dyn Any> {
//         self
//     }
//
//     fn as_any(&self) -> &dyn Any {
//         self
//     }
// }
//
// impl Clone for Box<dyn AnyClone> {
//     fn clone(&self) -> Box<dyn AnyClone> {
//         self.clone_box()
//     }
// }

// Non DMA version

// type TheSpi = embedded_hal_bus::spi::ExclusiveDevice<
//     esp_hal::spi::master::Spi<'static, esp_hal::Async>,
//     esp_hal::gpio::Output<'static>,
//     embedded_hal_bus::spi::NoDelay,
// >;

// DMA vers>n

type TheSpi = embedded_hal_bus::spi::ExclusiveDevice<
    esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
    esp_hal::gpio::Output<'static>,
    embedded_hal_bus::spi::NoDelay,
>;

#[allow(private_interfaces)]
pub struct Store {
    framework: Rc<RefCell<Framework>>,
    observers: RefCell<Vec<alloc::rc::Weak<RefCell<dyn StoreObserver>>>>,
    // pub requests_channel: &'static StoreRequestsChannel,
    // TODO: make spools_db mutext or something that doesn't need borrow
    // Think if need to make the entire store under mutex (if there are several related dbs could case issues)
    pub spools_db: OnceCell<CsvDb<SpoolRecord, TheSpi, 20, 5>>,
    last_spool_id: RefCell<i32>,
    spool_tag_id_index: RefCell<HashMap<String, String>>,
    pub initialized: RefCell<bool>,
    store_rc: RefCell<Option<Rc<Store>>>,
    pub storage_config: RefCell<StorageConfig>,

    pub locations_db: OnceCell<CsvDb<TagLocationRecord, TheSpi, 20, 5>>,
    last_location_id: RefCell<i32>,
}

impl Store {
    pub fn new(framework: Rc<RefCell<Framework>>) -> Rc<Store> {
        // let requests_channel = mk_static!(StoreRequestsChannel, StoreRequestsChannel::new());
        let store = Rc::new(Self {
            framework: framework.clone(),
            observers: RefCell::new(Vec::new()),
            // requests_channel,
            spools_db: OnceCell::new(),
            last_spool_id: RefCell::new(0),
            spool_tag_id_index: RefCell::new(HashMap::new()),
            initialized: RefCell::new(false),
            store_rc: RefCell::new(None),
            storage_config: RefCell::new(StorageConfig::default()),
            locations_db: OnceCell::new(),
            last_location_id: RefCell::new(0),
        });
        *store.store_rc.borrow_mut() = Some(store.clone());
        store
    }

    pub fn start(&self, view_model: Rc<RefCell<ViewModel>>) {
        let store = self.store_rc.borrow_mut().clone().unwrap();
        self.framework
            .borrow()
            .spawner
            .spawn_heap(store_task(self.framework.clone(), store, view_model))
            .ok();
    }

    pub fn subscribe(&self, observer: alloc::rc::Weak<RefCell<dyn StoreObserver>>) {
        self.observers.borrow_mut().push(observer);
    }

    pub fn is_available(&self) -> bool {
        self.spools_db.get().is_some()
    }
    pub fn is_initialized(&self) -> bool {
        *self.initialized.borrow()
    }

    pub async fn try_restore_from_backup(&self, view_model: Rc<RefCell<ViewModel>>) -> Result<(), String> {
        info!("Trying to restore from backup if '/store.bak' exists");
        let file_store = self.framework.borrow().file_store();
        let mut file_store = file_store.lock().await;

        // check if there is store.bak
        let file_exist = file_store
            .file_exists("/store.bak")
            .await
            .map_err(|e| format!("Error checking if '/store.bak' exists : {e}"))?;

        if !file_exist {
            info!("file '/store.bak' doesn't exist, no need to restore");
            return Ok(());
        }

        let store_folder_exist = file_store
            .dir_exists("/STORE")
            .await
            .map_err(|e| format!("Error checking if '/STORE' exists : {e}"))?;

        // now check if store folder exist
        if store_folder_exist {
            let mut store_copy_id = 1;
            loop {
                let store_copy_folder_exist = file_store
                    .dir_exists(&format!("/STORE.{store_copy_id}"))
                    .await
                    .map_err(|e| format!("Error checking if '/STORE.{store_copy_id}' exists : {e}"))?;
                if !store_copy_folder_exist {
                    break;
                }
                store_copy_id += 1;
            }
            if let Err(err) = file_store.rename_entry_in_dir("/", "STORE", &format!("STORE.{store_copy_id}")).await {
                error!("file '/store.bak' exists, '/STORE' folder also exists and couldn't be renamed : {err:?}");
                view_model.borrow().message_box(
                    "Restore Inventory Notice",
                    "Found '/store.bak' and couldn't rename '/STORE' folder ",
                    "Remove '/STORE' folder manually to restore or remove '/store.bak' to avoid this message",
                    crate::app::StatusType::Error,
                    0,
                );
                return Ok(());
            } else {
                info!("Renamed '/STORE' folder to '/STORE.{store_copy_id}'");
            }
        }

        let backup_data = match file_store.read_file_bytes("/store.bak").await {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        let mut pos = 0;
        let backup_data = backup_data.as_slice();
        #[allow(unused_variables)]
        let backup_meta = if let Some(next) = backup_data[pos..].iter().position(|&b| b == b'\n') {
            let _backup_meta = match serde_json::from_slice::<BackupMeta>(&backup_data[..next]) {
                Ok(v) => v,
                Err(err) => {
                    error!("Error in backup header: {err}");
                    view_model.borrow().message_box(
                        "Restoring Inventory",
                        "Error in '/store.bak' File Content",
                        "Unrecognized File Header Information",
                        crate::app::StatusType::Error,
                        0,
                    );
                    return Err("Error in /store.bak".to_string());
                }
            };
            pos += next + 1;
            _backup_meta
        } else {
            error!("Error parsing backup meta, \\n not found");
            view_model.borrow().message_box(
                "Restoring Inventory",
                "Error in /store.bak File Content",
                "Expected '\\n' Character When Searching For Header",
                crate::app::StatusType::Error,
                0,
            );
            return Err("Error in /store.bak".to_string());
        };
        while let Some(next) = backup_data[pos..].iter().position(|&b| b == b'\n') {
            let file_meta = match serde_json::from_slice::<FileMeta>(&backup_data[pos..pos + next]) {
                Ok(v) => v,
                Err(err) => {
                    error!("Error in file info header: {err}");
                    error!("Bytes data: {:?}", &backup_data[pos..pos + next]);
                    error!(
                        "String data: {}",
                        core::str::from_utf8(&backup_data[pos..pos + next]).unwrap_or("NOT Utf8")
                    );
                    view_model.borrow().message_box(
                        "Restoring Inventory",
                        "Error in '/store.bak' File Content",
                        "Failed to Parse a File Details Part",
                        crate::app::StatusType::Error,
                        0,
                    );
                    return Err("Error in /store.bak".to_string());
                }
            };
            pos += next + 1; // skip also \n

            let file_content = &backup_data[pos..pos + file_meta.length];
            match file_store.create_write_file_bytes(&file_meta.path, file_content).await {
                Ok(_) => (),
                Err(err) => {
                    error!("Error writing file {} : {err:?}", file_meta.path);
                    view_model.borrow().message_box(
                        "Restoring Inventory",
                        &format!("Error Writing {}", file_meta.path),
                        &format!("{err:?}"),
                        crate::app::StatusType::Error,
                        0,
                    );
                    return Err(format!("Error writing file {}", file_meta.path));
                }
            }
            info!("Restoring file: {file_meta:?}");
            view_model.borrow().message_box(
                "Restoring Inventory",
                &format!("Restoring File\n{}", file_meta.path),
                &format!("Progress: {}%", 100 * pos / backup_data.len()),
                crate::app::StatusType::Normal,
                0,
            );
            pos += file_meta.length + 1; // skip also \n
        }

        if let Err(err) = file_store.delete_file("/store.bak").await {
            error!("Error deleting /store.bak : {err:?}");
            view_model.borrow().message_box(
                "Restoring Inventory",
                "Inventory Restore Completed, But Failed to Delete '/store.bak'",
                &format!("{err:?}"),
                crate::app::StatusType::Error,
                0,
            );
        } else {
            view_model.borrow().message_box(
                "Restoring Inventory",
                "Inventory Restore Completed Successfully",
                "",
                crate::app::StatusType::Success,
                0,
            );
        }

        Ok(())
    }

    //// Spools Database Methods

    pub fn query_spools(&self) -> Option<String> {
        if let Some(spools_db) = self.spools_db.get() {
            let spool_records = spools_db.records.borrow();
            let total_length = spool_records.values().map(|v| v.length).sum::<usize>();
            let results: Result<String, CsvDbError> = spool_records.values().try_fold(String::with_capacity(total_length), |mut acc, v| {
                let csv = v.to_csv_string();
                if let Err(e) = &csv {
                    error!("Error serializing to csv: {v:?} : {e}");
                }
                acc.push_str(&csv?);
                Ok(acc)
            });
            // TODO: make it an error up as well, to handle in the caller
            results.ok()
        } else {
            None
        }
    }

    pub async fn delete_spool(&self, id: &str) -> Result<(), StoreError> {
        let deleted_record = if let Some(spools_db) = &self.spools_db.get() {
            let delete_res = spools_db.delete(id).await;
            if let Ok(Some(record)) = &delete_res {
                self.remove_all_spool_tags_from_tag_id_index(&record.id);
            }
            delete_res.context(CsvDbSnafu)?
        } else {
            None
        };

        if let Some(deleted_record) = deleted_record
            && let Ok(spool_rec_ext_file_path) = spool_rec_ext_file_path(&deleted_record.id)
        {
            let file_store = self.framework.borrow().file_store();
            let mut file_store = file_store.lock().await;
            let _ = file_store.delete_file(&spool_rec_ext_file_path).await;
        }
        Ok(())
    }

    pub fn remove_spool_tag_from_tag_id_index(&self, tag_id: &str) -> Option<String> {
        let res = self.spool_tag_id_index.borrow_mut().remove(tag_id);
        if res.is_some() {
            // if was key previously (and now isn't) need to send an update on remove
            for weak_observer in self.observers.borrow().iter() {
                let observer = weak_observer.upgrade().unwrap();
                observer.borrow().on_tag_removed();
            }
        }
        res
    }

    pub fn insert_spool_tag_to_tag_id_index(&self, tag_id: String, spool_id: String) -> Option<String> {
        let res = self.spool_tag_id_index.borrow_mut().insert(tag_id, spool_id);
        if res.is_none() {
            // if was no key (and now there is) need to send an update on add
            for weak_observer in self.observers.borrow().iter() {
                let observer = weak_observer.upgrade().unwrap();
                observer.borrow().on_tag_added();
            }
        }
        res
    }

    fn remove_all_spool_tags_from_tag_id_index(&self, spool_id: &str) {
        let tags_to_remove: Vec<String> = self
            .spool_tag_id_index
            .borrow()
            .iter()
            .filter(|(_, index_spool_id)| *index_spool_id == spool_id)
            .map(|(index_tag, _)| index_tag.clone())
            .collect();
        for tag_id in tags_to_remove {
            self.remove_spool_tag_from_tag_id_index(&tag_id);
        }
    }

    fn sync_spool_tags_in_tag_id_index(&self, spool_record: &SpoolRecord) {
        self.remove_all_spool_tags_from_tag_id_index(&spool_record.id);
        for tag_id in spool_record.linked_tag_ids() {
            self.insert_spool_tag_to_tag_id_index(tag_id.to_string(), spool_record.id.clone());
        }
    }

    pub async fn add_spool(&self, mut spool_rec: SpoolRecord, spool_rec_ext: SpoolRecordExt) -> Result<String, StoreError> {
        let new_spool_id = (*self.last_spool_id.borrow()) + 1;
        if let Some(spools_db) = &self.spools_db.get() {
            spool_rec.id = new_spool_id.to_string();
            if spool_rec.added_time.is_none() {
                spool_rec.added_time = store_safe_time_now();
            }
            spool_rec.ext_has_k = spool_rec_ext.k_info.is_some();
            let spool_rec_for_index = spool_rec.clone();
            match spools_db.insert(spool_rec).await.context(CsvDbSnafu)? {
                true => {
                    *self.last_spool_id.borrow_mut() = new_spool_id;
                    self.store_spool_rec_ext(&new_spool_id.to_string(), &spool_rec_ext).await?;
                    self.sync_spool_tags_in_tag_id_index(&spool_rec_for_index);
                    Ok(new_spool_id.to_string())
                }
                false => {
                    error!("Internal error, add spool added an already existing spool");
                    Err(StoreError::InternalError)
                }
            }
        } else {
            error!("Internal error, can't spools database");
            Err(StoreError::NoCsvDb)
        }
    }

    pub async fn edit_spool_from_web(&self, spool_record: SpoolRecord, k_info: Option<KInfo>) -> Result<(), StoreError> {
        if let Some(spools_db) = &self.spools_db.get() {
            let updated_record = {
                let spools_db_borrow = spools_db.records.borrow(); // Important: Note this borrow, dropped when context ends, but if changing need to make sure it is dropped
                if let Some(current_record) = spools_db_borrow.get(&spool_record.id) {
                    // Taking this approach with extra clones, so if future fields are added, this won't be missed
                    let current_record = &current_record.data;
                    SpoolRecord {
                        id: spool_record.id.clone(),
                        tag_id: spool_record.tag_id.clone(),
                        material_type: spool_record.material_type,
                        material_subtype: spool_record.material_subtype,
                        color_name: spool_record.color_name,
                        color_code: spool_record.color_code,
                        note: spool_record.note,
                        brand: spool_record.brand,
                        weight_advertised: spool_record.weight_advertised,
                        weight_core: spool_record.weight_core,
                        weight_new: current_record.weight_new,         // can't change from web
                        weight_current: current_record.weight_current, // can't change from web
                        slicer_filament: spool_record.slicer_filament,
                        added_time: current_record.added_time.or(store_safe_time_now()), // in case somehow no added date (ntp) then add it now
                        encode_time: current_record.encode_time,
                        added_full: spool_record.added_full,
                        consumed_since_add: current_record.consumed_since_add,
                        consumed_since_weight: current_record.consumed_since_weight,
                        ext_has_k: k_info.is_some(),
                        data_origin: current_record.data_origin.clone(),
                        tag_type: current_record.tag_type.clone(),
                        assigned_location: spool_record.assigned_location.clone(),
                        actual_location: spool_record.actual_location.clone(),
                        spools_count: spool_record.spools_count,
                    }
                } else {
                    return Err(StoreError::NotFound { id: spool_record.id.clone() });
                }
            };

            let updated_has_tag = updated_record.has_valid_tag_id();
            spools_db.insert(updated_record.clone()).await.context(CsvDbSnafu)?;
            self.sync_spool_tags_in_tag_id_index(&updated_record);

            let mut spool_rec_ext = match self.get_spool_ext_by_id(&spool_record.id).await {
                Ok(spool_rec_ext) => spool_rec_ext,
                Err(err) => {
                    error!("Error loading extended info when editing file, using empty as baseline for edit: {err:?}");
                    SpoolRecordExt::default()
                }
            };
            spool_rec_ext.k_info = k_info;
            if spool_rec_ext.tag.is_some() && !updated_has_tag {
                spool_rec_ext.tag = None;
            }
            self.store_spool_rec_ext(&spool_record.id, &spool_rec_ext).await?;
            Ok(())
        } else {
            Err(StoreError::NoCsvDb)
        }
    }
    pub fn get_spool_by_hex_tag(&self, tag_id_hex: &str) -> Option<SpoolRecord> {
        if let Some(spools_db) = self.spools_db.get()
            && let Some(spool_id) = self.spool_tag_id_index.borrow().get(tag_id_hex)
            && let Some(current_rec) = spools_db.records.borrow().get(spool_id)
        {
            return Some(current_rec.data.clone());
        }
        None
    }
    pub fn get_spool_by_tag_id(&self, tag_id: &[u8]) -> Option<SpoolRecord> {
        self.get_spool_by_hex_tag(&tag_id_hex(tag_id))
    }

    #[allow(dead_code)]
    pub fn exists_hex_tag_id(&self, tag_id_hex: &str) -> bool {
        self.spool_tag_id_index.borrow().contains_key(tag_id_hex)
    }
    #[allow(dead_code)]
    pub fn exists_tag_id(&self, tag_id: &[u8]) -> bool {
        self.exists_hex_tag_id(&hex::encode_upper(tag_id))
    }

    pub fn tags_in_store(&self) -> String {
        let mut tags_in_store = String::with_capacity(self.spool_tag_id_index.borrow().len() * (7 + 1) + 1);
        tags_in_store.push(','); // start with ","
        for tag in self.spool_tag_id_index.borrow().keys() {
            tags_in_store.push_str(tag);
            tags_in_store.push(',');
        }
        tags_in_store
    }

    pub fn get_spool_by_id(&self, id: &str) -> Option<SpoolRecord> {
        if let Some(spools_db) = self.spools_db.get()
            && let Some(current_rec) = spools_db.records.borrow().get(id)
        {
            return Some(current_rec.data.clone());
        }
        None
    }

    pub fn get_spool_csv_by_id(&self, id: &str) -> Option<String> {
        if let Some(spools_db) = self.spools_db.get()
            && let Some(current_rec) = spools_db.records.borrow().get(id)
            && let Ok(mut csv_str) = current_rec.to_csv_string()
        {
            // the to_csv_string adds a trailing \n automatically
            if csv_str.ends_with('\n') {
                csv_str.pop(); // removes last char
            }
            return Some(csv_str);
        }
        None
    }

    // TODO: once working, use it in other places reading ext
    pub async fn get_spool_ext_by_id(&self, id: &str) -> Result<SpoolRecordExt, StoreError> {
        if self.get_spool_by_id(id).is_none() {
            return Err(StoreError::NotFound { id: id.to_string() });
        }
        let spool_rec_ext_file_path = spool_rec_ext_file_path(id).map_err(|_| StoreError::NotFound { id: id.to_string() })?;
        let file_store = self.framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        let ext_str = file_store
            .read_file_str(&spool_rec_ext_file_path)
            .await
            .map_err(|err| StoreError::ExtFileReadFailure {
                error: format!("{err} reading '{spool_rec_ext_file_path}'"),
            })?;
        let mut de = Deserializer::from_str(&ext_str);
        let spool_rec_ext = SpoolRecordExt::deserialize(&mut de).context(ExtFormatSnafu)?;
        // let spool_rec_ext = serde_json::from_str::<SpoolRecordExt>(&ext_str).context(ExtFormatSnafu)?;
        Ok(spool_rec_ext)
    }

    #[allow(clippy::type_complexity)]
    pub async fn update_spool(
        &self,
        mut spool_record: SpoolRecord,
        update_ext_fn: Option<Box<dyn FnOnce(&mut SpoolRecordExt)>>,
    ) -> Result<Option<SpoolRecordExt>, StoreError> {
        let mut ret_spool_rec_ext = None;
        if let Some(spools_db) = self.spools_db.get() {
            if !spool_record.id.is_empty() {
                if spools_db.records.borrow().contains_key(&spool_record.id) {
                    if let Some(update_ext_fn) = update_ext_fn {
                        let mut spool_rec_ext = self.get_spool_ext_by_id(&spool_record.id).await.ok().unwrap_or_default(); // on read error don't raise error
                        update_ext_fn(&mut spool_rec_ext);
                        spool_record.ext_has_k = spool_rec_ext.k_info.is_some();
                        self.store_spool_rec_ext(&spool_record.id, &spool_rec_ext).await?;
                        ret_spool_rec_ext = Some(spool_rec_ext);
                    }
                    let spool_record_for_index = spool_record.clone();
                    // TODO: ? theoretically need transaction mechanism here (so lock db and then do the index operation as well)
                    spools_db.insert(spool_record).await.context(CsvDbSnafu)?;
                    self.sync_spool_tags_in_tag_id_index(&spool_record_for_index);
                    Ok(ret_spool_rec_ext)
                } else {
                    error!("Internal error, can't access store");
                    Err(StoreError::NoCsvDb)
                }
            } else {
                Err(StoreError::IdNotFound)
            }
        } else {
            Err(StoreError::MissingId)
        }
    }

    pub async fn store_spool_rec_ext(&self, id: &str, spool_rec_ext: &SpoolRecordExt) -> Result<String, StoreError> {
        let spool_rec_ext_file_path = spool_rec_ext_file_path(id)?;
        let file_store = self.framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        let s = serde_json::to_string(&spool_rec_ext).map_err(|_err| StoreError::InternalError)?;
        file_store.create_write_file_str(&spool_rec_ext_file_path, &s).await.context(StoreSnafu)?;
        Ok(spool_rec_ext_file_path)
    }

    #[allow(unused_variables)]
    pub async fn upgrade_versions(
        &self,
        db_version: semver::Version,
        current_version: semver::Version,
        view_model: Rc<RefCell<ViewModel>>,
    ) -> Result<bool, StoreError> {
        let mut spool_issues = String::new();
        if let Some(spools_db) = self.spools_db.get() {
            let mut spool_ids: Vec<_> = {
                let records = spools_db.records.borrow();
                records.keys().cloned().collect()
            };
            spool_ids.sort_by_key(|s| s.parse::<u32>().ok());
            let num_of_spools = spool_ids.len();
            for (index, spool_id) in spool_ids.iter().enumerate() {
                info!("Upgrading store spool # {spool_id}, {index} / {num_of_spools}");
                view_model.borrow().message_box(
                    "Store Notice",
                    &format!("Upgrading Spool # {spool_id}"),
                    &format!("{index}/{num_of_spools}"),
                    crate::app::StatusType::Normal,
                    0,
                );
                let mut spool_rec_ext = SpoolRecordExt::default();
                match self.get_spool_ext_by_id(spool_id.as_str()).await {
                    Ok(loaded_spool_rec_ext) => {
                        spool_rec_ext = loaded_spool_rec_ext;
                        if let Some(tag_desciptor) = &spool_rec_ext.tag {
                            match TagInformationV1::from_v1_descriptor(tag_desciptor) {
                                Ok(tag_info) => {
                                    if !tag_info.calibrations.is_empty() {
                                        let k_info = view_model.borrow().get_k_info_from_old_tag(&tag_info);
                                        if let Some(k_info) = k_info {
                                            info!("Upgrading spool {}, adding k_info {:?} to extended info", spool_id, k_info);
                                            spool_rec_ext.k_info = Some(k_info);
                                        }
                                    }
                                }
                                Err(err) => {
                                    error!("Error parsing tag descriptor for spool {}, ignoring : {err:?}", spool_id);
                                    spool_issues.push_str(&format!("Error parsing tag descriptor for spool {spool_id}, ignoring : {err:?}\n"));
                                    // Store anyway, since there were issues with old files that needs to be fixed
                                }
                            }
                        } else {
                            warn!("No tag descriptor found for spool {}, ignoring", spool_id);
                            spool_issues.push_str(&format!("No tag descriptor found for spool {spool_id}, ignoring\n"));
                        }
                    }
                    Err(_err) => (),
                }
                // Store anyway, since there were issues with old files that needs to be fixed (writing small file on larger file leave extra in file)
                // and potentially past versions with missing files
                if let Err(err) = self.store_spool_rec_ext(spool_id, &spool_rec_ext).await {
                    // TODO: undo upgrade and restore old version of file system?
                    error!("Error storing ext data for spool {}, ignoring : {err:?}", spool_id);
                    spool_issues.push_str(&format!("Error storing ext data for spool {}, ignoring : {err:?}\n", spool_id));
                } else {
                    spools_db.records.borrow_mut().get_mut(spool_id.as_str()).unwrap().data.ext_has_k = spool_rec_ext.k_info.is_some();
                }
            }
            spools_db.save_all_records_only_before_use().await.context(CsvDbSnafu)?;
            spools_db.update_version(SPOOLS_STORE_VER).await.context(CsvDbSnafu)?;
        }
        if !spool_issues.is_empty() {
            let file_store = self.framework.borrow().file_store();
            let mut file_store = file_store.lock().await;
            if let Err(err) = file_store.create_write_file_str("/STORE/upgrade.log", &spool_issues).await {
                error!("Error writing upgrade issues log");
            }
        }
        Ok(spool_issues.is_empty())
    }

    // Locations Records

    pub fn get_location_by_hex_tag(&self, tag_id_hex: &str) -> Option<TagLocationRecord> {
        if let Some(locations_db) = self.locations_db.get()
            && let Some(current_rec) = locations_db.records.borrow().get(tag_id_hex)
        {
            return Some(current_rec.data.clone());
        }
        None
    }

    pub async fn delete_location(&self, tag_id_hex: &str) -> Result<Option<TagLocationRecord>, StoreError> {
        if let Some(locations_db) = &self.locations_db.get() {
            Ok(locations_db.delete(tag_id_hex).await.context(CsvDbSnafu)?)
        } else {
            error!("Internal error, can't access locations database");
            Err(StoreError::NoCsvDb)
        }
    }

    pub async fn insert_tag_location(&self, tag_id_hex: &str, location: &str) -> Result<bool, StoreError> {
        assert!(!tag_id_hex.is_empty());
        assert!(!location.is_empty());
        if let Some(locations_db) = &self.locations_db.get() {
            let location_rec = TagLocationRecord {
                tag_id_hex: tag_id_hex.to_string(),
                location: location.to_string(),
            };
            Ok(locations_db.insert(location_rec).await.context(CsvDbSnafu)?)
        } else {
            error!("Internal error, can't access locations database");
            Err(StoreError::NoCsvDb)
        }
    }

    // Locations Storage

    pub async fn set_storage_config(&self, new_storage_config: StorageConfig) -> Result<String, String> {
        let file_store = self.framework.borrow().file_store();
        let mut file_store = file_store.lock().await;
        let storage_config_str = serde_json::to_string(&new_storage_config).unwrap();
        match file_store.create_write_file_str("/store/storcfg.jsn", &storage_config_str).await {
            Ok(_) => {
                *self.storage_config.borrow_mut() = new_storage_config;
                info!("Stored Spools Storage Configuration to /store/storcfg.jsn");
                Ok(storage_config_str)
            }
            Err(err) => {
                error!("Error storing Spools Storage Configuration to /store/storcfg.jsn");
                Err(format!("Error storing Spools Storage Configuration {err}"))
            }
        }
    }
}

// #[embassy_executor::task]
pub async fn store_task(framework: Rc<RefCell<Framework>>, store: Rc<Store>, view_model: Rc<RefCell<ViewModel>>) {
    let spools_db_available;
    let locations_db_available;
    {
        match store.try_restore_from_backup(view_model.clone()).await {
            Ok(_) => (),
            Err(e) => {
                term_error!(
                    "Inventory Restore started but failed at a critical point, inventory not available : {}",
                    e
                );
                view_model.borrow().message_box(
                    "Store Notice",
                    "Inventory Restore started but failed\nCheck terminal for more info",
                    &e.to_string(),
                    crate::app::StatusType::Error,
                    0,
                );
                loop {
                    Timer::after_secs(60).await;
                }
            }
        }
        debug!("Started store_task");
        let file_store = framework.borrow().file_store();

        {
            let mut file_store = file_store.lock().await;
            if let Ok(storage_config_str) = file_store.read_file_str("/store/storcfg.jsn").await {
                match serde_json::from_str::<StorageConfig>(&storage_config_str) {
                    Ok(storage_config) => {
                        *store.storage_config.borrow_mut() = storage_config;
                        term_info!("Loaded spools storage configuration (storage racks)");
                    }
                    Err(err) => {
                        term_error!("Error loading Spools Storage configuration (storage racks): {}", err);
                        view_model.borrow().message_box(
                            "Store Notice",
                            "Error Loading Storage Configuration",
                            &err.to_string(),
                            crate::app::StatusType::Error,
                            0,
                        );
                    }
                }
            } else {
                info!("No Spools Storage Configuration (Storage Racks) file");
            }
        }

        match CsvDb::<TagLocationRecord, _, FILE_STORE_MAX_DIRS, FILE_STORE_MAX_FILES>::new(
            file_store.clone(),
            "/store/locatags",
            1024,
            200,
            LOCATIONS_STORE_VER,
        )
        .await
        {
            Ok(mut db) => match db.start(true, true).await {
                Ok(_) => {
                    let db_version = {
                        let db_inner = db.inner.borrow();
                        db_inner.db_meta.version.clone()
                    };
                    match semver::Version::parse(db_version.as_str()) {
                        Ok(db_version) => {
                            let current_version = semver::Version::parse(SPOOLS_STORE_VER).unwrap();
                            if current_version < db_version {
                                term_info!(
                                    "Critical Error: Locations DB version is {}, this firmware supports up to {}",
                                    db_version,
                                    current_version
                                );
                                locations_db_available = false;
                            } else {
                                // currently upgrade is only for ext, so done after loading the db
                                store
                                    .locations_db
                                    .set(db)
                                    .map_err(|_e| "Fatal Internal Error: Can't assign locations_db to once_cell?")
                                    .unwrap();
                                term_info!("Opened locations database");

                                locations_db_available = true;
                            }
                        }
                        Err(err) => {
                            term_error!("Unparsable locations database version {} {:?}", db_version, err);
                            locations_db_available = false;
                        }
                    }
                }
                Err(e) => {
                    term_error!("Failed to start locations database (and load data): {:?}", e);
                    locations_db_available = false;
                }
            },
            Err(e) => {
                term_error!("Failed to open locations database : {}", e);
                locations_db_available = false;
            }
        }

        match CsvDb::<SpoolRecord, _, FILE_STORE_MAX_DIRS, FILE_STORE_MAX_FILES>::new(
            file_store.clone(),
            "/store/spools",
            1024,
            200,
            SPOOLS_STORE_VER,
        )
        .await
        {
            Ok(mut db) => match db.start(true, true).await {
                Ok(_) => {
                    let mut db_version = {
                        let db_inner = db.inner.borrow();
                        db_inner.db_meta.version.clone()
                    };
                    if db_version == "1" {
                        db_version = "1.0.0".to_string();
                    }
                    match semver::Version::parse(db_version.as_str()) {
                        Ok(db_version) => {
                            let current_version = semver::Version::parse(SPOOLS_STORE_VER).unwrap();
                            if current_version < db_version {
                                term_info!(
                                    "Critical Error: Spools database version is {}, this firmware supports up to {}",
                                    db_version,
                                    current_version
                                );
                                spools_db_available = false;
                            } else {
                                // currently upgrade is only for ext, so done after loading the db
                                store
                                    .spools_db
                                    .set(db)
                                    .map_err(|_e| "Fatal Internal Error: Can't assign spools_db to once_cell?")
                                    .unwrap();
                                term_info!("Opened spools database");

                                if current_version > db_version {
                                    info!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
                                    view_model.borrow().message_box(
                                        "Store Notice",
                                        "Upgrading Store",
                                        &format!("From Version {} to {}", db_version, current_version),
                                        crate::app::StatusType::Normal,
                                        0,
                                    );
                                    term_info!("Upgrading Spools database from {} to {}", db_version, current_version);
                                    info!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
                                    match store.upgrade_versions(db_version, current_version, view_model.clone()).await {
                                        Ok(status) => {
                                            info!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
                                            let (upgrade_notice1, upgrade_notice2, upgrade_status) = {
                                                if status {
                                                    (
                                                        "Spools DB Upgrade Completed Successfuly",
                                                        "No Issues Reported",
                                                        crate::app::StatusType::Success,
                                                    )
                                                } else {
                                                    (
                                                        "Spools DB Upgrade Completed With Issues",
                                                        "See /STORE/upgrade.log for details",
                                                        crate::app::StatusType::Normal,
                                                    )
                                                }
                                            };
                                            view_model
                                                .borrow()
                                                .message_box("Store Notice", upgrade_notice1, upgrade_notice2, upgrade_status, 0);
                                            term_info!(upgrade_notice1);
                                            term_info!(upgrade_notice2);
                                            info!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
                                            spools_db_available = true;
                                        }
                                        Err(err) => {
                                            info!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
                                            term_error!("Error upgrading store : {:?}", err);
                                            view_model.borrow().message_box(
                                                "Store Notice",
                                                "Error Upgrading Spools DB",
                                                &err.to_string(),
                                                crate::app::StatusType::Error,
                                                0,
                                            );
                                            info!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
                                            spools_db_available = false;
                                        }
                                    }
                                } else {
                                    spools_db_available = true;
                                }
                            }
                        }
                        Err(err) => {
                            term_error!("Unparsable spools database version {} {:?}", db_version, err);
                            spools_db_available = false;
                        }
                    }
                }
                Err(e) => {
                    term_error!("Failed to start spools database (and load data): {:?}", e);
                    spools_db_available = false;
                }
            },
            Err(e) => {
                term_error!("Failed to open spools database : {}", e);
                spools_db_available = false;
            }
        }
    }

    // create tag indexes and find largest_id's in lists, would be better if we persisted that

    let mut largest_location_id = 0;
    if locations_db_available && let Some(locations_db) = store.spools_db.get() {
        let records = locations_db.records.borrow();
        for record in records.iter() {
            if let Ok(id) = record.1.data.id.parse::<i32>()
                && id > largest_location_id
            {
                largest_location_id = id;
            }
        }
    }
    *store.last_location_id.borrow_mut() = largest_location_id;

    let mut largest_spool_id = 0;
    if spools_db_available && let Some(spools_db) = store.spools_db.get() {
        let records = spools_db.records.borrow();
        for record in records.iter() {
            if let Ok(id) = record.1.data.id.parse::<i32>() {
                for tag_id in record.1.data.linked_tag_ids() {
                    store.spool_tag_id_index.borrow_mut().insert(tag_id.to_string(), record.1.data.id.clone());
                }
                if id > largest_spool_id {
                    largest_spool_id = id;
                }
            }
        }
    }
    *store.last_spool_id.borrow_mut() = largest_spool_id;

    *store.initialized.borrow_mut() = true;

    // let receiver = store.requests_channel.receiver();
    loop {
        Timer::after_secs(60).await;
        // match receiver.receive().await {
        // }
    }
}

pub trait StoreObserver {
    fn on_tag_added(&self);
    fn on_tag_removed(&self);
    // fn on_read_spool_record_ext(&mut self, result: Result<SpoolRecordExt, String>);
}

fn tag_id_hex(tag_id: &[u8]) -> String {
    hex::encode_upper(tag_id)
}

fn spool_rec_ext_file_path(ext_rec_id: &str) -> Result<String, StoreError> {
    if let Ok(id_num) = ext_rec_id.parse::<i32>() {
        let folder_num = ((id_num / 16) % 16) + 1;
        let file_path = format!("/store/spools.ext/{folder_num}/{id_num}.jsn");
        Ok(file_path)
    } else {
        Err(StoreError::BadId)
    }
}

pub fn store_safe_time_now() -> Option<i32> {
    Instant::now().to_date_time().map(|date_time| date_time.timestamp() as i32)
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FileMeta {
    pub path: String,
    pub length: usize,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct BackupMeta {
    pub spoolease_console_ver: String,
}
