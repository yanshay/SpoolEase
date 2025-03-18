use core::cell::RefCell;
use core::ops::{Deref, DerefMut};

use alloc::string::String;
use alloc::{format, rc::Rc, string::ToString, vec::Vec};
use embassy_executor::Spawner;
use embassy_net::Stack;
use esp_mbedtls::TlsReference;
use hashbrown::HashMap;
use slint::{ComponentHandle, Model, SharedString, ToSharedString};

use framework::prelude::*;
use framework::{
    framework::{FrameworkObserver, WebConfigMode},
    terminal::{self, term_mut, TerminalObserver},
};

use crate::bambu::{ssdp_task, BambuSSDPInfo};
use crate::settings::MAX_NUM_PRINTERS;
use crate::{
    app_config::AppConfig,
    bambu::{self, BambuPrinter, BambuPrinterObserver, TagInformation, TrayState},
    filament_staging::FilamentStaging,
    spool_tag::{self, SpoolTagObserver, Status},
};

struct PrinterUiState {
    curr_ams: Option<i32>,
}
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
    // bambu_printer_model: Rc<RefCell<bambu::BambuPrinter>>,
    bambu_printer_model: SelectedPrinter,
    spool_tag_model: Rc<RefCell<spool_tag::SpoolTag>>,
    filament_staging: Rc<RefCell<FilamentStaging>>,
    spawner: Spawner,
    tls: TlsReference<'static>,
    printers_view_state: HashMap<String, PrinterUiState>,
}

impl ViewModel {
    pub fn new(
        // Framework
        stack: Stack<'static>,
        ui_weak: slint::Weak<crate::app::AppWindow>,
        framework: Rc<RefCell<Framework>>,
        // Application
        app_config: Rc<RefCell<AppConfig>>,
        // bambu_printer_model: Rc<RefCell<bambu::BambuPrinter>>,
        spool_tag_model: Rc<RefCell<spool_tag::SpoolTag>>,
        spawner: Spawner,
        tls: TlsReference<'static>,
    ) -> Rc<RefCell<ViewModel>> {
        let terminal_view_model = Rc::new(RefCell::new(TerminalViewModel { ui_weak: ui_weak.clone() }));
        let trait_for_terminal_rc: alloc::rc::Rc<core::cell::RefCell<dyn terminal::TerminalObserver>> = terminal_view_model.clone();
        let trait_for_terminal_weak: alloc::rc::Weak<core::cell::RefCell<dyn terminal::TerminalObserver>> =
            alloc::rc::Rc::downgrade(&trait_for_terminal_rc);
        term_mut().subscribe(trait_for_terminal_weak);

        let set_of_printers: Vec<Rc<RefCell<BambuPrinter>>> = Vec::new();
        // set_of_printers.push(bambu_printer_model.clone());
        let selected_printer = SelectedPrinter::new(set_of_printers, 0);

        let view_model_rc = Rc::new(RefCell::new(ViewModel {
            // Framework
            stack,
            ui_weak: ui_weak.clone(),
            view_model: None,
            framework: framework.clone(),
            _terminal_view_model: terminal_view_model, // used by Terminal with weak reference, hold it so it won't be released
            // Application
            // bambu_printer_model: bambu_printer_model.clone(),
            bambu_printer_model: selected_printer,
            spool_tag_model: spool_tag_model.clone(),
            app_config: app_config.clone(),
            filament_staging: Rc::new(RefCell::new(FilamentStaging::new())),
            spawner,
            tls,
            printers_view_state: HashMap::new(),
        }));

        // let trait_for_bambu_printer_rc: alloc::rc::Rc<core::cell::RefCell<dyn bambu::BambuPrinterObserver>> = view_model_rc.clone();
        // let trait_for_bambu_printer_weak: alloc::rc::Weak<core::cell::RefCell<dyn bambu::BambuPrinterObserver>> =
        //     alloc::rc::Rc::downgrade(&trait_for_bambu_printer_rc);
        // bambu_printer_model.borrow_mut().subscribe(trait_for_bambu_printer_weak);

        let trait_for_spool_tag_rc: alloc::rc::Rc<core::cell::RefCell<dyn spool_tag::SpoolTagObserver>> = view_model_rc.clone();
        let trait_for_spool_tag_weak: alloc::rc::Weak<core::cell::RefCell<dyn spool_tag::SpoolTagObserver>> =
            alloc::rc::Rc::downgrade(&trait_for_spool_tag_rc);
        spool_tag_model.borrow_mut().subscribe(trait_for_spool_tag_weak);

        let trait_for_framework_rc: alloc::rc::Rc<core::cell::RefCell<dyn FrameworkObserver>> = view_model_rc.clone();
        let trait_for_framework_weak: alloc::rc::Weak<core::cell::RefCell<dyn FrameworkObserver>> = alloc::rc::Rc::downgrade(&trait_for_framework_rc);
        framework.borrow_mut().subscribe(trait_for_framework_weak);

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
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_reset_fixed_security_key(move || {
                let _ = framework.borrow_mut().set_fixed_key("");
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

        let moved_filament_staging = self.filament_staging.clone();
        let moved_ui = self.ui_weak.clone();
        self.ui_weak.unwrap().global::<crate::app::AppBackend>().on_clear_staging(move || {
            moved_filament_staging.borrow_mut().clear();
            moved_ui.unwrap().global::<crate::app::AppState>().invoke_empty_spool_staging();
        });

        let moved_spool_tag = self.spool_tag_model.clone();
        let moved_ui = self.ui_weak.clone();
        moved_ui.unwrap().global::<crate::app::AppBackend>().on_cancel_encode(move || {
            moved_spool_tag.borrow().cancel_operation();
        });

        let ssdp_pub_sub = mk_static!(
            embassy_sync::pubsub::PubSubChannel::<embassy_sync::blocking_mutex::raw::NoopRawMutex, BambuSSDPInfo, 3, MAX_NUM_PRINTERS, 1>,
            embassy_sync::pubsub::PubSubChannel::<embassy_sync::blocking_mutex::raw::NoopRawMutex, BambuSSDPInfo, 3, MAX_NUM_PRINTERS, 1>::new()
        );

        self.spawner.spawn(ssdp_task(self.stack, ssdp_pub_sub)).ok();

        let mut default_printer_set = false;
        let mut printer_number = 0;
        let mut available_printers: Vec<SharedString> = Vec::new();
        for printer_config in &self.app_config.borrow().configured_printers.printers {
            let printer_serial = printer_config.serial.clone().unwrap();
            let printer_access_code = printer_config.access_code.clone().unwrap();
            let printer_name = printer_config.name.clone();
            let printer_ip = printer_config.ip.clone();

            let bambu_printer_model = bambu::init(
                printer_number.clone(),
                printer_serial.clone(),
                printer_access_code,
                printer_name.clone(),
                printer_ip,
                self.stack,
                self.app_config.clone(),
                self.tls,
                self.spawner,
                ssdp_pub_sub,
            );
            printer_number += 1;
            self.bambu_printer_model.printers.push(bambu_printer_model.clone());
            if !default_printer_set && Some(&printer_serial) == self.app_config.borrow().configured_default_printer.serial.as_ref() {
                // set the first with default serial to be the default (in case of using the same printer several times, for testing ...)
                self.bambu_printer_model.index = self.bambu_printer_model.printers.len() - 1;
                default_printer_set = true;
            }
            available_printers.push(bambu_printer_model.borrow().printer_selector_name.to_shared_string());

            // notification from printer on events, should be treated for all printers,
            // but selected printer should be considered as to what to update in the UI
            if let Some(view_model_rc) = &self.view_model {
                let trait_for_bambu_printer_rc: alloc::rc::Rc<core::cell::RefCell<dyn bambu::BambuPrinterObserver>> = view_model_rc.clone();
                let trait_for_bambu_printer_weak: alloc::rc::Weak<core::cell::RefCell<dyn bambu::BambuPrinterObserver>> =
                    alloc::rc::Rc::downgrade(&trait_for_bambu_printer_rc);
                bambu_printer_model.borrow_mut().subscribe(trait_for_bambu_printer_weak);
            }
        }
        let default_printer = self.bambu_printer_model.printers[self.bambu_printer_model.index]
            .borrow()
            .printer_selector_name
            .to_shared_string();
        let available_printers = slint::ModelRc::new(slint::VecModel::from(available_printers));
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppState>()
            .invoke_set_printers_info(available_printers, default_printer.clone());
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppState>()
            .invoke_set_curr_printer(default_printer);
        self.register_printer_related_listeners();

        let moved_ui = self.ui_weak.clone();
        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        // this select_printer handler CAN'T depend on printer because then it would need to change itself while running
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_select_printer(move |selected_printer: SharedString| {

                // First stored UI for this printer for when we switch back to it
                Self::perform_select_printer(moved_ui.clone(), moved_view_model.clone(), &selected_printer);
            });
    }


    fn perform_select_printer(moved_ui: slint::Weak<crate::app::AppWindow>, moved_view_model: Rc<RefCell<ViewModel>>, selected_printer: &SharedString) {
        // Collect printer view state to store until we switch back
        let current_shown_ams = moved_ui.unwrap().global::<crate::app::AppState>().get_curr_ams_id();
        let current_printer_selector_name = moved_ui.unwrap().global::<crate::app::AppState>().get_curr_printer();
        moved_view_model.borrow_mut().printers_view_state.insert(
            current_printer_selector_name.to_string(),
            PrinterUiState {
                curr_ams: Some(current_shown_ams),
            },
        );

        // Then process select
        let mut borrowed_view_model = moved_view_model.borrow_mut();
        let selected_printer_string = selected_printer.to_string();
        for (i, printer) in borrowed_view_model.bambu_printer_model.printers.iter().enumerate() {
            if &selected_printer_string == &printer.borrow().printer_selector_name {
                moved_ui
                    .unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_set_curr_printer(selected_printer.to_shared_string());
                borrowed_view_model.bambu_printer_model.index = i;
                moved_ui.unwrap().global::<crate::app::AppState>().set_curr_ams_id(0); // while strange, this is importnat here for restoring curr_ams after, next call will set it to the first (in case 0 doesn't exist)
                borrowed_view_model.update_ui_from_printer(&borrowed_view_model.bambu_printer_model.printers[i].borrow());
                // now we'll resrore to the corret curr_ams if user was already there before, if not it will stay on the correct first ams
                if let Some(printer_view_state) = &borrowed_view_model.printers_view_state.get(&selected_printer_string) {
                    if let Some(past_curr_ams_id) = printer_view_state.curr_ams {
                        moved_ui.unwrap().global::<crate::app::AppState>().set_curr_ams_id(past_curr_ams_id);
                    }
                }
                borrowed_view_model.register_printer_related_listeners();
                break;
            }
        }
    }

    fn set_staging_to_tray_direct(
        &mut self,
        filament_staging: &Rc<RefCell<FilamentStaging>>,
        bambu_printer: &mut BambuPrinter,
        ui: &slint::Weak<crate::app::AppWindow>,
        tray_id: i32,
    ) {
        let mut filament_staging = filament_staging.borrow_mut();
        if let Some(tag_info) = &filament_staging.tag_info {
            bambu_printer.set_tray_filament(tray_id, tag_info);
            filament_staging.clear();
            ui.unwrap().global::<crate::app::AppState>().invoke_empty_spool_staging();
            let (ams_id, tray_id) = BambuPrinter::get_ams_and_tray_id(tray_id as usize);
            let ams_id = ams_id as i32;
            let tray_id = tray_id as i32;
            ui.unwrap().global::<crate::app::AppState>().invoke_tray_update_succeeded(
                bambu_printer.printer_selector_name.to_shared_string(),
                ams_id,
                tray_id,
            );
        }
    }

    fn set_staging_to_tray(
        filament_staging: &Rc<RefCell<FilamentStaging>>,
        bambu_printer: &Rc<RefCell<BambuPrinter>>,
        ui: &slint::Weak<crate::app::AppWindow>,
        tray_id: i32,
    ) {
        let mut filament_staging = filament_staging.borrow_mut();
        if let Some(tag_info) = &filament_staging.tag_info {
            bambu_printer.borrow_mut().set_tray_filament(tray_id, tag_info);
            filament_staging.clear();
            ui.unwrap().global::<crate::app::AppState>().invoke_empty_spool_staging();
            let (ams_id, tray_id) = BambuPrinter::get_ams_and_tray_id(tray_id as usize);
            let ams_id = ams_id as i32;
            let tray_id = tray_id as i32;

            let selected_in_ui = ui.unwrap().global::<crate::app::AppState>().get_curr_printer();
            warn!(
                "UI Selected Printer: [{}], setting tray of printer: [{}]",
                selected_in_ui,
                bambu_printer.borrow().printer_selector_name
            );

            ui.unwrap().global::<crate::app::AppState>().invoke_tray_update_succeeded(
                bambu_printer.borrow().printer_selector_name.to_shared_string(),
                ams_id,
                tray_id,
            );
        }
    }

    fn register_printer_related_listeners(&mut self) {
        // handler for request from UI to move to staging, need to work only on selected printer
        let moved_filament_staging = self.filament_staging.clone();
        let moved_bambu_printer = self.bambu_printer_model.clone();
        let moved_ui = self.ui_weak.clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_set_staging_to_tray(move |tray_id: i32| {
                Self::set_staging_to_tray(&moved_filament_staging, &moved_bambu_printer, &moved_ui, tray_id);
            });

        // handler for request from UI to encode a spool, need to work only on selected printer
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
                let tray_id = usize::try_from(tray_id).unwrap();
                let borrowed_filament_staging = moved_filament_staging.borrow();
                let printer_tag_info: Option<TagInformation>;
                let tag_info = if tray_id == 999 {
                    // Encode from Staging
                    if let Some(staging_tag_info) = &borrowed_filament_staging.tag_info {
                        staging_tag_info
                    } else {
                        return 10;
                    }
                } else {
                    match moved_bambu_printer.borrow().get_tag_info_to_encode(tray_id) {
                        Ok(tag_info) => {
                            printer_tag_info = Some(tag_info);
                            printer_tag_info.as_ref().unwrap()
                        }
                        Err(err) => {
                            // hopefully no borrowing issues since calling into ui in a callback
                            moved_ui
                                .unwrap()
                                .global::<crate::app::AppState>()
                                .invoke_encoding_failed(err.to_shared_string());
                            return 10;
                        }
                    }
                };
                let bambu_printer_borrow = moved_bambu_printer.borrow();
                if let Some(descriptor) = &tag_info.to_descriptor(&bambu_printer_borrow.printer_name, &bambu_printer_borrow.printer_uuid_to_encode) {
                    spool_tag.write_tag(&descriptor, tray_id);
                }
                info!("Sent the write request of tray {} over signal", tray_id);
                // TODO: Get proper timeout fron config and pass it in the write_tag to spool_tag
                10
            });

        // handler for request from UI to reset printer, should work only on selected printer
        let moved_bambu_printer = self.bambu_printer_model.clone();
        let moved_ui = self.ui_weak.clone();
        self.ui_weak.unwrap().global::<crate::app::AppBackend>().on_reset_printer(move || {
            moved_bambu_printer.borrow_mut().reset_printer();
            moved_ui.unwrap().global::<crate::app::AppState>().invoke_reset_printer();
        });
    }

    fn tag_info_to_ui_spool_info(&self, tag_info: &TagInformation) -> Option<crate::app::UiSpoolInfo> {
        if tag_info.filament.is_none() {
            return None;
        }

        let filament_info = tag_info.filament.as_ref().unwrap();

        let color = u32::from_str_radix(&filament_info.tray_color[..6], 16).unwrap() + 0xFF000000; // the plus 0xFF at the end is fo add alpha

        let bambu_printer_borrow = self.bambu_printer_model.borrow();
        let mut final_k = bambu_printer_borrow.get_tag_k_for_current_nozzle(tag_info);
        if let Some(calibration) = tag_info
            .calibrations
            .get(bambu_printer_borrow.nozzle_diameter.as_ref().unwrap_or(&"NA".to_string()))
        {
            let source_k = &calibration.k_value;
            if source_k != &final_k {
                final_k = format!("?{final_k}");
            }
        }
        let ui_spool_info = crate::app::UiSpoolInfo {
            color: slint::Color::from_argb_encoded(color),
            k: SharedString::from(final_k),
            material: filament_info.tray_type.to_shared_string(),
        };
        Some(ui_spool_info)
    }

    fn update_ui_from_printer(&self, bambu_printer: &BambuPrinter) {
        // note - accepting bambu_printer rather than taking from self, because it may be called during callback on_trays_update,
        // and that's taking place when it's already borrowed and another borrow will panic

        let ui = self.ui_weak.unwrap();

        // ----- handle number of ams's and curr_ams -----
        if let Some(mut ams_exist_bits) = bambu_printer.ams_exist_bits {
            let mut ams_exist_vec = Vec::<i32>::new();
            let mut first_ams = -1;
            for ams_id in 0..=3 {
                if ams_exist_bits & 1 != 0 {
                    ams_exist_vec.push(ams_id);
                    if first_ams == -1 {
                        first_ams = ams_id;
                    }
                }
                ams_exist_bits >>= 1;
            }
            let ams_exists: Rc<slint::VecModel<i32>> = Rc::new(slint::VecModel::from(ams_exist_vec));
            let ams_exists = slint::ModelRc::from(ams_exists);
            ui.global::<crate::app::AppState>().set_ams_exists(ams_exists);
            let current_shown_ams = ui.global::<crate::app::AppState>().get_curr_ams_id();
            if first_ams > current_shown_ams {
                ui.global::<crate::app::AppState>().set_curr_ams_id(first_ams);
            }
        }

        // ----- handle trays view update ----
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
            ui_tray.tagged = curr_tray.tag_info.is_some();
            // let k_value_unformatted = curr_tray.k.as_ref().unwrap_or(&"(0.020)".to_string()).clone();
            let k_value_unformatted = bambu_printer.get_tray_resolved_k_value(&curr_tray);
            // let k_value_for_ui = k_value_for_ui(&k_value_unformatted);
            ui_tray.k = SharedString::from(k_value_unformatted);
            trays_state.set_row_data(tray_row, ui_tray);
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
    fn on_trays_update(&mut self, bambu_printer: &mut BambuPrinter, prev_trays_reading_bits: Option<u32>, new_trays_reading_bits: Option<u32>) {
        // note - accepting bambu_printer rather than taking from self, because it's already borrowed and another borrow will panic
        let current_selected_printer = self.bambu_printer_model.index;

        if bambu_printer.printer_number == current_selected_printer {
            self.update_ui_from_printer(bambu_printer);
        }

        // ----- Handle loading when there is something in staging -----
        // If the staging is loaded and only a SINGLE slot SWITCHED to reading update it to the stating filament info
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
            // if bambu_printer.printer_number == 0 { // UNREMARK FOR TESTS WITH ONE PRINTER
                if trays_reading_changed.len() == 1 {
                    let only_reading_tray = trays_reading_changed[0];
                    info!("Single tray {only_reading_tray} is loading now");
                    self.set_staging_to_tray_direct(
                        &self.filament_staging.clone(),
                        bambu_printer,
                        &self.ui_weak.clone(),
                        only_reading_tray as i32,
                    );
                }
            // }
        }
    }

    fn on_printer_connect_status(&self, bambu_printer: &mut BambuPrinter, status: bool) {
        if status {
            // TODO: I can't borrow at this stage because my_mqtt reports this and need to borrow_mut so now can't borrow.
            //       Need to switch to the notifications coming from a notifier object and not directly from the objects.
            //       Or switch to a message loop notifications (which is a major change to the code, but more correct for these types of apps)
            //       So here I know it arrives here only if boot is successful, but in other applications this might not be enough
            // if self.app_config.borrow().boot_completed() {
            term_info!(&"-".repeat(67));
            term_info!("Printer [{}] connected successfully", bambu_printer.printer_number);
            term_info!(&"-".repeat(67));
            self.ui_weak.unwrap().global::<crate::app::AppState>().invoke_printer_connected(bambu_printer.printer_selector_name.to_shared_string());
        }
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
            Status::WriteSuccess(pure_tray_id, encoded_descriptor) => {
                let (ams_id, tray_id) = BambuPrinter::get_ams_and_tray_id(*pure_tray_id);
                let ams_id = ams_id as i32;
                let tray_id = tray_id as i32;

                if let Ok(tag_info) = TagInformation::from_descriptor(encoded_descriptor) {
                    if let Some(ui_spool_info) = self.tag_info_to_ui_spool_info(&tag_info) {
                        self.filament_staging.borrow_mut().tag_info = Some(tag_info);
                        ui.unwrap().global::<crate::app::AppState>().invoke_update_spool_staging(ui_spool_info);
                        ui.unwrap().global::<crate::app::AppState>().invoke_encoding_succeeded(ams_id, tray_id);
                    } else {
                        ui.unwrap()
                            .global::<crate::app::AppState>()
                            .invoke_encoding_failed(SharedString::from("Descriptor Generation Error"));
                    }
                }
            }
            Status::ReadSuccess(read_text) => {
                if let Ok(tag_info) = TagInformation::from_descriptor(read_text) {
                    if let Some(ui_spool_info) = self.tag_info_to_ui_spool_info(&tag_info) {
                        self.filament_staging.borrow_mut().tag_info = Some(tag_info);
                        ui.unwrap().global::<crate::app::AppState>().invoke_read_tag_succeeded(ui_spool_info);
                    } else {
                        ui.unwrap()
                            .global::<crate::app::AppState>()
                            .invoke_read_tag_failed(SharedString::from("Invalid Tag Content"));
                    }
                }
            }
            Status::Failure(spool_tag::Failure::TagWriteFailure) => {
                ui.unwrap().global::<crate::app::AppState>().invoke_encoding_failed("".to_shared_string());
            }
            Status::Failure(spool_tag::Failure::TagReadFailure) => {
                ui.unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_read_tag_failed(SharedString::from("Error: Failed to Scan Tag"));
            }
        }
    }
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
            term_info!("Initialization completed successfully");
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

struct TerminalViewModel {
    ui_weak: slint::Weak<crate::app::AppWindow>,
}

impl TerminalObserver for TerminalViewModel {
    fn on_add_text(&self, text: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_add_term_text(text.to_shared_string());
    }
}

struct SelectedPrinter {
    printers: Vec<Rc<RefCell<BambuPrinter>>>,
    index: usize,
}

impl SelectedPrinter {
    fn new(vec: Vec<Rc<RefCell<BambuPrinter>>>, default_index: usize) -> Self {
        Self {
            printers: vec,
            index: default_index,
        }
    }
}

impl Deref for SelectedPrinter {
    type Target = Rc<RefCell<BambuPrinter>>;
    fn deref(&self) -> &Self::Target {
        &self.printers[self.index]
    }
}

impl DerefMut for SelectedPrinter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.printers[self.index]
    }
}
