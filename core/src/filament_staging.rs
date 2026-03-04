use alloc::{rc::Rc, string::String};
use framework::error;

use crate::spool_record::{FullSpoolRecord, SpoolRecord, SpoolRecordExt};
use crate::store::Store;

pub struct FilamentStaging {
    // tag_info: Option<TagInformation>,
    full_spool_rec: Option<FullSpoolRecord>,
    scanned_tag_id: Option<String>,
    // spool_record: Option<SpoolRecord>,
    // spool_record_ext: Option<SpoolRecordExt>,
    origin: StagingOrigin,
    _store: Rc<Store>,
}

#[derive(PartialEq)]
pub enum StagingOrigin {
    Empty,
    Scanned,
    Encoded,
    Unloaded,
}

impl FilamentStaging {
    pub fn new(store: Rc<Store>) -> Self {
        Self {
            full_spool_rec: None,
            scanned_tag_id: None,
            origin: StagingOrigin::Empty,
            _store: store.clone(),
        }
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.full_spool_rec.is_none()
    }

    pub fn clear(&mut self) {
        self.full_spool_rec = None;
        self.scanned_tag_id = None;
        self.origin = StagingOrigin::Empty;
    }
    // pub fn tag_info(&self) -> &Option<TagInformation> {
    //     &self.tag_info
    // }
    pub fn full_spool_rec(&self) -> &Option<FullSpoolRecord> {
        &self.full_spool_rec
    }
    pub fn spool_rec(&self) -> Option<&SpoolRecord> {
        self.full_spool_rec.as_ref().map(|f| &f.spool_rec)
    }
    pub fn _spool_rec_ext(&self) -> Option<&SpoolRecordExt> {
        self.full_spool_rec.as_ref().map(|f| &f.spool_rec_ext)
    }
    pub fn set_spool_record_ext(&mut self, spool_record_ext: SpoolRecordExt) {
        if let Some(full_spool_rec) = &mut self.full_spool_rec {
            full_spool_rec.spool_rec_ext = spool_record_ext;
        } else {
            error!("Internal Error storing spool_record_ext when full_spool_rec is empty");
        }
    }
    pub fn set_spool_record(&mut self, spool_rec: SpoolRecord, origin: StagingOrigin) {
        // if let Some(tag_info) = &mut self.tag_info {
        //     tag_info.id = Some(spool_rec.id.clone());
        // }
        self.full_spool_rec = Some(FullSpoolRecord {
            spool_rec,
            spool_rec_ext: SpoolRecordExt::default(),
        });
        self.scanned_tag_id = None;
        self.origin = origin;
    }

    pub fn set_scanned_tag_id(&mut self, scanned_tag_id: Option<String>) {
        self.scanned_tag_id = scanned_tag_id;
    }

    pub fn scanned_tag_id(&self) -> Option<&str> {
        self.scanned_tag_id.as_deref()
    }
    pub fn update_spool_rec_keep_rest(&mut self, spool_rec: SpoolRecord) {
        if let Some(full_spool_rec) = &mut self.full_spool_rec {
            full_spool_rec.spool_rec = spool_rec
        }
    }
    // pub fn _tag_info_mut(&mut self) -> &mut Option<TagInformation> {
    //     &mut self.tag_info
    // }
    // pub fn set_tag_info(&mut self, mut tag_info: Box<TagInformation>, origin: StagingOrigin) {
    //     // if loaded in scanning scenario or unloading scenario, the store should reflect some of the fields
    //     // also store_record is automatically fetched if available
    //
    //     // TODO: why store record is not fetched in case of encoded?
    //     // maybe it's because at that point it is not available yet?
    //     // or could it bring previous information of previous spool using that tag?
    //
    //     if [StagingOrigin::Scanned, StagingOrigin::Unloaded].contains(&origin) {
    //         if let Some(tag_id) = &tag_info.tag_id {
    //             if let Some(spool_in_store) = self.store.get_spool_by_tag_id(tag_id) {
    //                 tag_info.note = Some(spool_in_store.note.clone());
    //                 self.set_spool_record(spool_in_store);
    //             }
    //         }
    //     }
    //     self.tag_info = Some(*tag_info);
    //     self.origin = origin;
    // }
    pub fn origin(&self) -> &StagingOrigin {
        &self.origin
    }
}
