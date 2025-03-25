use core::cell::RefCell;

use alloc::{rc::Rc, string::String};
use framework::prelude::Framework;

pub struct AppConfig {
    framework: Rc<RefCell<Framework>>
}

impl AppConfig {
    pub fn new(framework: Rc<RefCell<Framework>> ) -> Self {
        Self { framework }
    }
    pub fn load_config_flash_then_toml(&mut self, _toml_str: &str) -> Result<(), String> {
        Ok(())
    }

    pub fn initialization_ok(&self) -> bool {
        self.framework.borrow().initialization_ok()
    }
}
