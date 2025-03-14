use crate::bambu::TagInformation;

pub struct FilamentStaging {
    pub tag_info: Option<TagInformation>,
}

impl FilamentStaging {
    pub fn new() -> Self {
        Self {
            tag_info: None,
        }
    }

    pub fn clear(&mut self) {
        self.tag_info = None
    }
}
