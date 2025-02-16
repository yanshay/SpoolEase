use core::{cell::RefCell, str::FromStr};

use alloc::{
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use embassy_net::Stack;
use slint::{ComponentHandle, Model, SharedString, ToSharedString};

use framework::prelude::*;
use framework::{ framework::{FrameworkObserver, WebConfigMode}, terminal::{self, term_mut, TerminalObserver} };

use crate::{
    app_config::{self, AppConfig, AppControlObserver},
    bambu::{self, BambuPrinter, BambuPrinterObserver, Filament, FilamentInfo, TrayState},
    filament_staging::FilamentStaging,
    spool_tag::{self, SpoolTagObserver, Status},
};

pub struct ViewModel {
    // Framework
    stack: Stack<'static>,
    ui_weak: slint::Weak<crate::app::AppWindow>,
    view_model: Option<Rc<RefCell<Self>>>,
    framework: Rc<RefCell<Framework>>,
    _terminal_view_model: Rc<RefCell<TerminalViewModel>>,
    // Application
    #[allow(dead_code)]
    app_config: Rc<RefCell<AppConfig>>,
    bambu_printer_model: Rc<RefCell<bambu::BambuPrinter>>,
    spool_tag_model: Rc<RefCell<spool_tag::SpoolTag>>,
    filament_staging: Rc<RefCell<FilamentStaging>>,
}

impl ViewModel {
    pub fn new(
        // Framework
        stack: Stack<'static>,
        ui_weak: slint::Weak<crate::app::AppWindow>,
        framework: Rc<RefCell<Framework>>,
        // Application
        app_config: Rc<RefCell<AppConfig>>,
        bambu_printer_model: Rc<RefCell<bambu::BambuPrinter>>,
        spool_tag_model: Rc<RefCell<spool_tag::SpoolTag>>,
    ) -> Rc<RefCell<ViewModel>> {
        let terminal_view_model = Rc::new(RefCell::new(TerminalViewModel {
            ui_weak: ui_weak.clone()
        }));
        let trait_for_terminal_rc: alloc::rc::Rc<core::cell::RefCell<dyn terminal::TerminalObserver>> = terminal_view_model.clone();
        let trait_for_terminal_weak: alloc::rc::Weak<core::cell::RefCell<dyn terminal::TerminalObserver>> =
            alloc::rc::Rc::downgrade(&trait_for_terminal_rc);
        term_mut().subscribe(trait_for_terminal_weak);

        let view_model_rc = Rc::new(RefCell::new(ViewModel {
            // Framework
            stack,
            ui_weak: ui_weak.clone(),
            view_model: None,
            framework: framework.clone(),
            _terminal_view_model: terminal_view_model, // used by Terminal with weak reference, hold it so it won't be released
            // Application
            bambu_printer_model: bambu_printer_model.clone(),
            spool_tag_model: spool_tag_model.clone(),
            app_config: app_config.clone(),
            filament_staging: Rc::new(RefCell::new(FilamentStaging::new())),
        }));

        let trait_for_bambu_printer_rc: alloc::rc::Rc<core::cell::RefCell<dyn bambu::BambuPrinterObserver>> = view_model_rc.clone();
        let trait_for_bambu_printer_weak: alloc::rc::Weak<core::cell::RefCell<dyn bambu::BambuPrinterObserver>> =
            alloc::rc::Rc::downgrade(&trait_for_bambu_printer_rc);
        bambu_printer_model.borrow_mut().subscribe(trait_for_bambu_printer_weak);

        let trait_for_spool_tag_rc: alloc::rc::Rc<core::cell::RefCell<dyn spool_tag::SpoolTagObserver>> = view_model_rc.clone();
        let trait_for_spool_tag_weak: alloc::rc::Weak<core::cell::RefCell<dyn spool_tag::SpoolTagObserver>> =
            alloc::rc::Rc::downgrade(&trait_for_spool_tag_rc);
        spool_tag_model.borrow_mut().subscribe(trait_for_spool_tag_weak);

        let trait_for_framework_rc: alloc::rc::Rc<core::cell::RefCell<dyn FrameworkObserver>> = view_model_rc.clone();
        let trait_for_framework_weak: alloc::rc::Weak<core::cell::RefCell<dyn FrameworkObserver>> =
            alloc::rc::Rc::downgrade(&trait_for_framework_rc);
        framework.borrow_mut().subscribe(trait_for_framework_weak);


        let trait_for_app_control_rc: alloc::rc::Rc<core::cell::RefCell<dyn app_config::AppControlObserver>> = view_model_rc.clone();
        let trait_for_app_control_weak: alloc::rc::Weak<core::cell::RefCell<dyn app_config::AppControlObserver>> =
            alloc::rc::Rc::downgrade(&trait_for_app_control_rc);
        app_config.borrow_mut().subscribe(trait_for_app_control_weak);

        view_model_rc.borrow_mut().view_model = Some(view_model_rc.clone());
        view_model_rc
    }

    pub fn init_framework(&mut self) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .set_app_info(crate::app::AppInfo {
                name: env!("CARGO_PKG_NAME").into(),
                version: env!("CARGO_PKG_VERSION").into(),
            });

        let framework = self.framework.clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_reset_flash_wifi_credentials(move || {
                framework.borrow_mut().erase_stored_wifi_credentials();
                framework.borrow_mut().reset_device();
            });

        let framework = self.framework.clone();
        let stack = self.stack;
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_start_web_config(move || {
                framework.borrow().start_web_app(stack, WebConfigMode::STA);
            });

        let framework = self.framework.clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_stop_web_config(move || {
                framework.borrow().stop_web_app();
            });

        let framework = self.framework.clone();
        self.ui_weak.unwrap().global::<crate::app::FrameworkBackend>().on_reset_device(move || {
            framework.borrow().reset_device();
        });

        let framework = self.framework.clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_update_firmware_ota(move || {
                framework.borrow().update_firmware_ota();
            });
    }

    pub fn init(&mut self) {
        self.init_framework(); // Initialization of framework

        // initialization of application (consider moving to a separate function)

        let moved_filament_staging = self.filament_staging.clone();
        let moved_ui = self.ui_weak.clone();
        self.ui_weak.unwrap().global::<crate::app::AppBackend>().on_clear_staging(move || {
            moved_filament_staging.borrow_mut().clear();
            moved_ui.unwrap().global::<crate::app::AppState>().invoke_empty_spool_staging();
        });

        let moved_filament_staging = self.filament_staging.clone();
        let moved_bambu_printer = self.bambu_printer_model.clone();
        let moved_ui = self.ui_weak.clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_set_staging_to_tray(move |tray_id: i32| {
                Self::set_staging_to_tray(&moved_filament_staging, &moved_bambu_printer, &moved_ui, tray_id);
            });

        let moved_filament_staging = self.filament_staging.clone();
        let moved_bambu_printer = self.bambu_printer_model.clone();
        let moved_spool_tag = self.spool_tag_model.clone();
        let moved_ui = self.ui_weak.clone();
        moved_ui
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_encode_tray_to_tag(move |tray_id| {
                info!("Request to encode tag with {tray_id} info");
                let spool_tag = moved_spool_tag.borrow();
                let bambu_printer = moved_bambu_printer.borrow();
                let tray_id = usize::try_from(tray_id).unwrap();
                let filament = if tray_id == 999 {
                    // Staging
                    &moved_filament_staging.borrow().filament_info
                } else if tray_id == 254 {
                    // External
                    &bambu_printer.virt_tray.filament
                } else {
                    &bambu_printer.ams_trays[tray_id].filament
                };
                if let Filament::Known(f) = filament {
                    spool_tag.write_tag(&f.to_descriptor(), tray_id);
                    info!("Sent the write request of tray {} over signal", tray_id);
                }
                // TODO: Get proper timeout fron config and pass it in the write_tag to spool_tag
                10
            });

        let moved_spool_tag = self.spool_tag_model.clone();
        let moved_ui = self.ui_weak.clone();
        moved_ui.unwrap().global::<crate::app::AppBackend>().on_cancel_encode(move || {
            moved_spool_tag.borrow().cancel_operation();
        });
    }

    fn set_staging_to_tray(
        filament_staging: &Rc<RefCell<FilamentStaging>>,
        bambu_printer: &Rc<RefCell<BambuPrinter>>,
        ui: &slint::Weak<crate::app::AppWindow>,
        tray_id: i32,
    ) {
        let mut filament_staging = filament_staging.borrow_mut();
        if let Filament::Known(ref filament_info) = &filament_staging.filament_info {
            bambu_printer.borrow().set_tray_filament(tray_id, filament_info);
            filament_staging.clear();
            ui.unwrap().global::<crate::app::AppState>().invoke_empty_spool_staging();
            let (ams_id, tray_id) = BambuPrinter::get_ams_and_tray_id(tray_id as usize);
            let ams_id = ams_id as i32;
            let tray_id = tray_id as i32;
            ui.unwrap().global::<crate::app::AppState>().invoke_tray_update_succeeded(ams_id, tray_id);
        }
    }
}

impl From<&TrayState> for crate::app::UiTrayState {
    fn from(v: &TrayState) -> crate::app::UiTrayState {
        match v {
            TrayState::Unknown => crate::app::UiTrayState::Unknown,
            TrayState::Empty => crate::app::UiTrayState::Empty,
            TrayState::Spool => crate::app::UiTrayState::Spool,
            TrayState::Reading => crate::app::UiTrayState::Reading,
            TrayState::Ready => crate::app::UiTrayState::Ready,
            TrayState::Loading => crate::app::UiTrayState::Loading,
            TrayState::Unloading => crate::app::UiTrayState::Unloading,
            TrayState::Loaded => crate::app::UiTrayState::Loaded,
        }
    }
}

impl BambuPrinterObserver for ViewModel {
    fn on_trays_update(&self, bambu_printer: &BambuPrinter, prev_trays_reading_bits: Option<u32>, new_trays_reading_bits: Option<u32>) {
        let ui = self.ui_weak.unwrap();

        if let Some(mut ams_exist_bits) = bambu_printer.ams_exist_bits {
            let mut ams_exist_vec = Vec::<i32>::new();
            let mut ams_id = 0;
            while ams_exist_bits != 0 {
                if ams_exist_bits & 1 != 0 {
                    ams_exist_vec.push(ams_id);
                    ams_exist_bits >>= 1;
                    ams_id += 1;
                }
            }
            let ams_exists: Rc<slint::VecModel<i32>> = Rc::new(slint::VecModel::from(ams_exist_vec));
            let ams_exists = slint::ModelRc::from(ams_exists);
            ui.global::<crate::app::AppState>().set_ams_exists(ams_exists);
        }

        let trays_state_rc = ui.global::<crate::app::AppState>().get_trays_state();
        // let trays_state_rc = ui.get_trays_state();
        let trays_state = trays_state_rc;
        for tray_row in 0..trays_state.row_count() {
            let tray_id = trays_state.row_data(tray_row).unwrap().id;
            let curr_tray = if tray_id == 254 {
                &bambu_printer.virt_tray
            } else {
                &bambu_printer.ams_trays[usize::try_from(tray_id).unwrap()]
            };
            let mut ui_tray = trays_state.row_data(tray_row).unwrap().clone();
            ui_tray.spool_state = crate::app::UiTrayState::from(&curr_tray.state);
            if let bambu::Filament::Known(filament_info) = &curr_tray.filament {
                // FIX: when color string is less than 6 chars
                let color = u32::from_str_radix(&filament_info.tray_color[..6], 16).unwrap() + 0xFF000000; // the plus at the end is fo add alpha
                ui_tray.filament.color = slint::Color::from_argb_encoded(color);
                ui_tray.filament.material = slint::SharedString::from(&filament_info.tray_type);
                ui_tray.filament.state = crate::app::UiFilamentState::Known;
            } else {
                ui_tray.filament.state = crate::app::UiFilamentState::Unknown;
            }
            let k_value_for_ui = curr_tray.k.as_ref().unwrap_or(&String::new()).clone();
            let k_value_for_ui = if k_value_for_ui.starts_with("(") {
                let k_value_for_ui = k_value_for_ui.trim_matches(['(', ')']);
                let k_value = f32::from_str(&k_value_for_ui).unwrap_or_default();
                format!("({:.3})", k_value)
            } else {
                let k_value = f32::from_str(&k_value_for_ui).unwrap_or_default();
                format!("{:.3}", k_value)
            };
            ui_tray.k = SharedString::from(k_value_for_ui);
            trays_state.set_row_data(tray_row, ui_tray);
        }

        // If the staging is loaded and only a SINGLE slot SWITCHED to reading update it to the stating filament info
        // TODO: Think if UI wise, we want to ask on the panel if to load or not, and not do automatically (maybe with timeout)
        if let Some(new_trays_reading_bits) = new_trays_reading_bits {
            let prev_trays_reading_bits = prev_trays_reading_bits.unwrap_or(0);
            let mut trays_reading_changed = Vec::new();
            for tray_id in 0..bambu_printer.ams_trays.len() {
                let prev_tray_reading_bit = ((prev_trays_reading_bits >> tray_id) & 0x01) != 0;
                let new_tray_reading_bit = ((new_trays_reading_bits >> tray_id) & 0x01) != 0;
                if prev_tray_reading_bit == false && new_tray_reading_bit == true {
                    trays_reading_changed.push(tray_id);
                }
            }
            if trays_reading_changed.len() == 1 {
                let only_reading_tray = trays_reading_changed[0];
                info!("Single tray {only_reading_tray} is loading now");
                ui.global::<crate::app::AppState>()
                    .invoke_new_single_tray_loading(only_reading_tray as i32);
            }
        }
        // }
    }
}

// TODO:
// Add support for technical PN532 severe errors reporting (when can't connect to device, etc.)
impl SpoolTagObserver for ViewModel {
    fn on_tag_status(&mut self, status: &Status) {
        self.framework.borrow().undim_display();
        let ui = self.ui_weak.clone();
        // let tag_timeout = self.app_config.borrow().tag_scan_timeout;
        match status {
            Status::FoundTagNowReading => {
                ui.unwrap().global::<crate::app::AppState>().invoke_read_tag_found();
            }
            Status::FoundTagNowWriting => {
                ui.unwrap().global::<crate::app::AppState>().invoke_encode_tag_found();
            }
            Status::WriteSuccess(pure_tray_id) => {
                let (ams_id, tray_id) = BambuPrinter::get_ams_and_tray_id(*pure_tray_id);
                let ams_id = ams_id as i32;
                let tray_id = tray_id as i32;
                ui.unwrap().global::<crate::app::AppState>().invoke_encoding_succeeded(ams_id, tray_id);

                let filament = if *pure_tray_id == 999 {
                    self.filament_staging.borrow().filament_info.clone()
                } else if *pure_tray_id == 254 {
                    let bambu_printer_model_clone = self.bambu_printer_model.clone();
                    let bambu_printer_model = bambu_printer_model_clone.borrow();
                    let tray = &bambu_printer_model.virt_tray;
                    tray.filament.clone()
                } else {
                    let bambu_printer_model_clone = self.bambu_printer_model.clone();
                    let bambu_printer_model = bambu_printer_model_clone.borrow();
                    let tray = &bambu_printer_model.ams_trays[*pure_tray_id];
                    tray.filament.clone()
                };
                if let Filament::Known(filament_info) = filament {
                    let ui_spool_info = filament_info_to_ui_spool_info(self.bambu_printer_model.borrow(), &filament_info);
                    ui.unwrap().global::<crate::app::AppState>().invoke_update_spool_staging(ui_spool_info);
                }
            }
            Status::ReadSuccess(read_text) => {
                let bambu_printer_model = self.bambu_printer_model.borrow();
                if let Ok(filament_info) = FilamentInfo::from_descriptor(read_text, &bambu_printer_model) {
                    let ui_spool_info = filament_info_to_ui_spool_info(bambu_printer_model, &filament_info);
                    self.filament_staging.borrow_mut().filament_info = Filament::Known(filament_info);

                    ui.unwrap().global::<crate::app::AppState>().invoke_read_tag_succeeded(ui_spool_info);
                } else {
                    ui.unwrap()
                        .global::<crate::app::AppState>()
                        .invoke_read_tag_failed(SharedString::from("Invalid Tag Info"));
                }
            }
            Status::Failure(spool_tag::Failure::TagWriteFailure) => {
                ui.unwrap().global::<crate::app::AppState>().invoke_encoding_failed();
            }
            Status::Failure(spool_tag::Failure::TagReadFailure) => {
                ui.unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_read_tag_failed(SharedString::from("Error: Failed to Scan Tag"));
            }
        }
    }
}

fn filament_info_to_ui_spool_info(bambu_printer_model: core::cell::Ref<'_, BambuPrinter>, filament_info: &FilamentInfo) -> crate::app::UiSpoolInfo {
    let color = u32::from_str_radix(&filament_info.tray_color[..6], 16).unwrap() + 0xFF000000;
    // the plus at the end is fo add alpha
    let ui_spool_info = crate::app::UiSpoolInfo {
        color: slint::Color::from_argb_encoded(color),
        k: SharedString::from(k_value_for_ui(&bambu_printer_model.get_filament_k_for_current_nozzle(filament_info))),
        material: SharedString::from(&filament_info.tray_type),
    };
    ui_spool_info
}

fn k_value_for_ui(k: &str) -> String {
    if k.is_empty() {
        "".to_string();
    }
    let k_value_for_ui = if k.starts_with("(") {
        let k = k.trim_matches(['(', ')']);
        let k_value = f32::from_str(k).unwrap_or_default();
        format!("({:.3})", k_value)
    } else {
        let k_value = f32::from_str(k).unwrap_or_default();
        format!("{:.3}", k_value)
    };
    k_value_for_ui
}

impl FrameworkObserver for ViewModel {
    fn on_web_config_started(&self, key: &str, mode: WebConfigMode) {
        let mode = match mode {
            WebConfigMode::AP => crate::app::WebConfigState::StartedAP,
            WebConfigMode::STA => crate::app::WebConfigState::StartedSTA,
        };
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_web_config_started(SharedString::from(key), mode);
    }

    fn on_web_config_stopped(&self) {
        self.ui_weak.unwrap().global::<crate::app::FrameworkState>().invoke_web_config_stopped();
    }
    fn on_wifi_sta_connected(&self) {
        self.framework.borrow().check_firmware_ota();
    }

    fn on_ota_start(&self) {
        self.ui_weak.unwrap().global::<crate::app::FrameworkState>().invoke_ota_started();
    }

    fn on_ota_status(&self, text: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_ota_status(SharedString::from(text));
    }

    fn on_ota_completed(&self, text: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_ota_completed(SharedString::from(text));
    }

    fn on_ota_failed(&self, text: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_ota_failed(SharedString::from(text));
    }

    fn on_ota_version_available(&self, version: &str, newer: bool) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_set_ota_info(crate::app::OtaInfo {
                version: version.to_shared_string(),
                newer,
            });
    }

    fn on_webapp_url_update(&self, url: &str, ssid: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_set_web_config_url(SharedString::from(url), SharedString::from(ssid));
    }

    fn on_initialization_completed(&self, status: bool) {
        if status {
            term_info!(&"-".repeat(66));
            term_info!("Initialization completed successfuly");
            term_info!(&"-".repeat(66));
        } else {
            // TODO: This event here goes to the AppState and not to Framework, think about that.
            self.ui_weak
                .unwrap()
                .global::<crate::app::AppState>()
                .invoke_boot_failed("Boot Failed\nScroll Up for Details".to_shared_string());
            term_info!(&"x".repeat(47));
            term_info!("Initialization failed - Review errors, fix, and restart");
            term_info!(&"x".repeat(47));
        }
    }
}

impl AppControlObserver for ViewModel {
    fn on_printer_connect_status(&self, status: bool) {
        if status {
            // TODO: I can't borrow at this stage because my_mqtt reports this and need to borrow_mut so now can't borrow.
            //       Need to switch to the notifications coming from a notifier object and not directly from the objects.
            //       Or switch to a message loop notifications (which is a major change to the code, but more correct for these types of apps)
            //       So here I know it arrives here only if boot is successful, but in other applications this might not be enough
            // if self.app_config.borrow().boot_completed() {
            term_info!(&"-".repeat(66));
            term_info!("Startup completed successfuly");
            term_info!(&"-".repeat(66));
            self.ui_weak.unwrap().global::<crate::app::AppState>().invoke_boot_succeeded();
            // }
        }
    }
}

struct TerminalViewModel {
    ui_weak: slint::Weak<crate::app::AppWindow>,
}

impl TerminalObserver for TerminalViewModel {
    fn on_add_text(&self, text: &str) {
        self.ui_weak.unwrap().global::<crate::app::FrameworkState>().invoke_add_term_text(text.to_shared_string());
    }
}
