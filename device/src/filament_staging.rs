use crate::bambu::Filament;

pub struct FilamentStaging {
    pub filament_info: Filament,
}

impl FilamentStaging {
    pub fn new() -> Self {
        Self {
            filament_info: Filament::Unknown,
        }
    }

    pub fn clear(&mut self) {
        self.filament_info = Filament::Unknown;
    }
}
