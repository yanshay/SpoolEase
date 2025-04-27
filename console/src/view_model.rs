use core::cell::RefCell;
use core::ops::{Deref, DerefMut};

use alloc::string::String;
use alloc::{
    format,
    rc::{Rc, Weak},
    string::ToString,
    vec::Vec,
};
use embassy_net::Stack;
use embedded_hal_bus::spi::ExclusiveDevice;
use hashbrown::HashMap;
use slint::{ComponentHandle, Model, SharedString, ToSharedString};

use framework::prelude::*;
use framework::{
    framework::{FrameworkObserver, WebConfigMode},
    terminal::{self, term_mut, TerminalObserver},
};

use crate::app_config::{BASE_FILAMENTS, SPOOLS_CATALOG};
use crate::spool_scale::{self, SpoolScaleObserver};
use crate::ssdp::{ssdp_task, SSDPPubSubChannel};
use crate::web_app::EncodeInfoDTO;
use crate::{
    app_config::AppConfig,
    bambu::{self, BambuPrinter, BambuPrinterObserver, TagInformation, TrayState},
    filament_staging::FilamentStaging,
};
use shared::spool_tag::{self, SpoolTagObserver, Status};

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
    bambu_printer_model: SelectedPrinter,
    spool_tag_model: Rc<RefCell<spool_tag::SpoolTag>>,
    spool_scale_model: Rc<RefCell<spool_scale::SpoolScale>>,
    filament_staging: Rc<RefCell<FilamentStaging>>,
    printers_view_state: HashMap<String, PrinterUiState>,

    cores_list_vec_rc: slint::ModelRc<crate::app::SelectorOption>,
    spools_cores_weights: HashMap<i32, i32>,
    spools_cores_filter: String,
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
        spi_device: ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
        irq: esp_hal::gpio::Input<'static>,
    ) -> Rc<RefCell<ViewModel>> {
        let spawner = framework.borrow().spawner;
        // Setup Terminal
        let terminal_view_model = Rc::new(RefCell::new(TerminalViewModel { ui_weak: ui_weak.clone() }));
        let trait_for_terminal_rc: Rc<RefCell<dyn terminal::TerminalObserver>> = terminal_view_model.clone();
        let trait_for_terminal_weak: Weak<RefCell<dyn terminal::TerminalObserver>> = Rc::downgrade(&trait_for_terminal_rc);
        term_mut().subscribe(trait_for_terminal_weak);

        // Setup empty printers
        let set_of_printers: Vec<Rc<RefCell<BambuPrinter>>> = Vec::new();
        let selected_printer = SelectedPrinter::new(set_of_printers, 0);

        // Initialize SpoolTag
        let spool_tag_model = spool_tag::init(spi_device, irq, spawner);

        // Initialize ssdp
        let ssdp_pub_sub = mk_static!(SSDPPubSubChannel, SSDPPubSubChannel::new());
        spawner.spawn(ssdp_task(stack, ssdp_pub_sub)).ok();

        // Initialize spool_scale_model
        let spool_scale_model = crate::spool_scale::init(framework.clone(), app_config.clone(), stack, spawner, ssdp_pub_sub);

        // Prepare an empty spool weights lists, later we'll replace it
        let spools_cores_weights: HashMap<i32, i32> = HashMap::with_capacity(300);
        let selector_options_vec: slint::VecModel<crate::app::SelectorOption> = slint::VecModel::default();
        let selector_options_vec_rc = slint::ModelRc::from(Rc::new(selector_options_vec));

        // Create the ViewModel
        let view_model = ViewModel {
            // Framework
            stack,
            ui_weak: ui_weak.clone(),
            view_model: None,
            framework: framework.clone(),
            _terminal_view_model: terminal_view_model, // used by Terminal with weak reference, hold it so it won't be released
            // Application
            bambu_printer_model: selected_printer,
            spool_tag_model: spool_tag_model.clone(),
            spool_scale_model: spool_scale_model.clone(),
            app_config: app_config.clone(),
            filament_staging: Rc::new(RefCell::new(FilamentStaging::new())),
            printers_view_state: HashMap::new(),
            cores_list_vec_rc: selector_options_vec_rc,
            spools_cores_weights,
            spools_cores_filter: String::new(),
        };
        let view_model_rc = Rc::new(RefCell::new(view_model));

        // hold a reference to itself to hand over to others, this is a 'memory leak' but object never gets destroyed so eaiser than weak reference
        view_model_rc.borrow_mut().view_model = Some(view_model_rc.clone());

        // Initialize
        view_model_rc.borrow_mut().init_framework_stuff();
        view_model_rc.borrow_mut().init_app_stuff(ssdp_pub_sub);

        // Done
        view_model_rc
    }

    pub fn init_framework_stuff(&mut self) {
        // Subscribe to rust structs framework events
        let trait_for_framework_rc: Rc<RefCell<dyn FrameworkObserver>> = self.view_model.as_ref().unwrap().clone();
        let trait_for_framework_weak: Weak<RefCell<dyn FrameworkObserver>> = Rc::downgrade(&trait_for_framework_rc);
        self.framework.borrow_mut().subscribe(trait_for_framework_weak);

        let ui = self.ui_weak.unwrap();

        // Initialize UI FrameworkState with framework information
        let ui_framework_state = ui.global::<crate::app::FrameworkState>();
        ui_framework_state.set_app_info(crate::app::AppInfo {
            name: env!("CARGO_PKG_NAME").into(),
            version: env!("CARGO_PKG_VERSION").into(),
        });

        // Register to UI (Slint) framework events (UI FrameworkBackend API's)
        let ui_framework_backend = ui.global::<crate::app::FrameworkBackend>();

        let framework = self.framework.clone();
        ui_framework_backend.on_reset_flash_wifi_credentials(move || {
            framework.borrow_mut().erase_stored_wifi_credentials();
            framework.borrow_mut().reset_device();
        });

        let framework = self.framework.clone();
        ui_framework_backend.on_reset_fixed_security_key(move || {
            let _ = framework.borrow_mut().set_fixed_key("");
        });

        let framework = self.framework.clone();
        let stack = self.stack;
        ui_framework_backend.on_start_web_config(move || {
            framework.borrow_mut().start_web_app(stack, WebConfigMode::STA);
        });

        let framework = self.framework.clone();
        ui_framework_backend.on_stop_web_config(move || {
            framework.borrow().stop_web_app();
        });

        let framework = self.framework.clone();
        ui_framework_backend.on_reset_device(move || {
            framework.borrow().reset_device();
        });

        let framework = self.framework.clone();
        ui_framework_backend.on_update_firmware_ota(move || {
            framework.borrow().update_firmware_ota();
        });
    }

    pub fn init_app_stuff(&mut self, ssdp_pub_sub: &'static SSDPPubSubChannel) {
        // Subscribe to rust spool_tag events
        let trait_for_spool_tag_rc: Rc<RefCell<dyn spool_tag::SpoolTagObserver>> = self.view_model.as_ref().unwrap().clone();
        let trait_for_spool_tag_weak: Weak<RefCell<dyn spool_tag::SpoolTagObserver>> = Rc::downgrade(&trait_for_spool_tag_rc);
        self.spool_tag_model.borrow_mut().subscribe(trait_for_spool_tag_weak);

        // Subscribe to rust spool_scale events
        let trait_for_spool_scale_rc: Rc<RefCell<dyn spool_scale::SpoolScaleObserver>> = self.view_model.as_ref().unwrap().clone();
        let trait_for_spool_scale_weak: Weak<RefCell<dyn spool_scale::SpoolScaleObserver>> = Rc::downgrade(&trait_for_spool_scale_rc);
        self.spool_scale_model.borrow_mut().subscribe(trait_for_spool_scale_weak);

        let ui = self.ui_weak.unwrap();
        let ui_app_backend = ui.global::<crate::app::AppBackend>();
        let ui_app_state = ui.global::<crate::app::AppState>();

        // Register to UI(Slint) app UI events
        let moved_filament_staging = self.filament_staging.clone();
        let moved_ui = self.ui_weak.clone();
        ui_app_backend.on_clear_staging(move || {
            moved_filament_staging.borrow_mut().clear();
            moved_ui.unwrap().global::<crate::app::AppState>().invoke_empty_spool_staging();
        });

        let moved_spool_tag = self.spool_tag_model.clone();
        ui_app_backend.on_read_tag_mode(move || {
            moved_spool_tag.borrow().read_tag();
        });

        let moved_spool_tag = self.spool_tag_model.clone();
        let moved_framework = self.framework.clone();
        ui_app_backend.on_emulate_tag_web_config(move || {
            let borrowed_framework = moved_framework.borrow();
            let web_config_ip_url = &borrowed_framework.web_config_ip_url;
            let web_config_key = &borrowed_framework.web_config_key;
            let full_web_config_url = format!("{web_config_ip_url}#sk={web_config_key}");
            moved_spool_tag.borrow().emulate_tag(&full_web_config_url);
        });

        let moved_spool_tag = self.spool_tag_model.clone();
        let moved_framework = self.framework.clone();
        ui_app_backend.on_emulate_tag_encoding_info(move || {
            let borrowed_framework = moved_framework.borrow();
            let web_config_ip_url = &borrowed_framework.web_config_ip_url;
            let web_config_key = &borrowed_framework.web_config_key;
            let full_web_config_url = format!("{web_config_ip_url}/encode.html#sk={web_config_key}");
            moved_spool_tag.borrow().emulate_tag(&full_web_config_url);
        });

        // Spool Scale
        let moved_spool_scale_model = self.spool_scale_model.clone();
        ui_app_backend.on_calibrate_scale(move |weight| {
            moved_spool_scale_model.borrow_mut().calibrate(weight);
        });

        let moved_spool_scale_model = self.spool_scale_model.clone();
        ui_app_backend.on_get_connected_scale_info(move || {
            let connected_scale = &moved_spool_scale_model.borrow().connected_scale;
            if let Some(connected_scale) = connected_scale {
                let scale_name = match &connected_scale.0 {
                    Some(s) if !s.is_empty() => s.as_str(),
                    _ => "<Unnamed Scale/IP set w/o name>",
                };
                format!("{} - {}", connected_scale.1, scale_name).to_shared_string()
            } else {
                "<No Scale Connected>".to_shared_string()
            }
        });

        let moved_spool_scale_model = self.spool_scale_model.clone();
        ui_app_backend.on_get_available_scales_info(move || {
            let available_scales = &moved_spool_scale_model.borrow().available_scales;
            let mut available_scales_res = Vec::<SharedString>::new();

            for scale in available_scales {
                let scale_name = match &scale.0 {
                    Some(s) if !s.is_empty() => s.as_str(),
                    _ => "<Unnamed Scale>",
                };
                available_scales_res.push(format!("{} - {}", scale.1, scale_name).to_shared_string());
            }
            slint::ModelRc::new(slint::VecModel::from(available_scales_res))
        });

        // Initialize Printers ///////////////////////////

        let mut default_printer_set = false;
        let mut printer_number = 1; // starts from one and incremented for any printer
        let mut printer_index = 0; // starts from zero and incremented only on successful init and adding to array
        let mut available_printers: Vec<SharedString> = Vec::new();
        for printer_config in &self.app_config.borrow().configured_printers.printers {
            match bambu::init(
                self.framework.clone(),
                printer_number,
                printer_index,
                &printer_config,
                self.app_config.clone(),
                ssdp_pub_sub,
            ) {
                Ok(bambu_printer_model) => {
                    self.bambu_printer_model.printers.push(bambu_printer_model.clone());
                    if !default_printer_set
                        && Some(&bambu_printer_model.borrow().printer_serial) == self.app_config.borrow().configured_default_printer.serial.as_ref()
                    {
                        // set the first with default serial to be the default (in case of using the same printer several times, for testing ...)
                        self.bambu_printer_model.index = self.bambu_printer_model.printers.len() - 1;
                        default_printer_set = true;
                    }
                    available_printers.push(bambu_printer_model.borrow().printer_selector_name.to_shared_string());

                    // notification from printer on events, should be treated for all printers,
                    // but selected printer should be considered as to what to update in the UI
                    if let Some(view_model_rc) = &self.view_model {
                        let trait_for_bambu_printer_rc: Rc<RefCell<dyn bambu::BambuPrinterObserver>> = view_model_rc.clone();
                        let trait_for_bambu_printer_weak: Weak<RefCell<dyn bambu::BambuPrinterObserver>> = Rc::downgrade(&trait_for_bambu_printer_rc);
                        bambu_printer_model.borrow_mut().subscribe(trait_for_bambu_printer_weak);
                    }
                    printer_index += 1; // index is increased only if printer is added to array
                }
                Err(e) => {
                    term_info!("[{}] Error initializing printer: {}", printer_number, e);
                }
            }
            printer_number += 1; // printer_number is always increased, even if printer is bad config
        }
        if !self.bambu_printer_model.printers.is_empty() {
            let default_printer = self.bambu_printer_model.printers[self.bambu_printer_model.index]
                .borrow()
                .printer_selector_name
                .to_shared_string();
            let available_printers = slint::ModelRc::new(slint::VecModel::from(available_printers));
            ui_app_state.invoke_set_printers_info(available_printers, default_printer.clone());
            ui_app_state.invoke_set_curr_printer(default_printer);
            self.register_printer_related_listeners();

            let moved_ui = self.ui_weak.clone();
            let moved_view_model = self.view_model.as_ref().unwrap().clone();
            // this select_printer handler CAN'T depend on printer because then it would need to change itself while running
            ui_app_backend.on_select_printer(move |selected_printer: SharedString| {
                // First stored UI for this printer for when we switch back to it
                Self::perform_select_printer(moved_ui.clone(), moved_view_model.clone(), &selected_printer);
            });
        }

        // Initialize SpoolScale and weight related stuff

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        ui_app_backend.on_get_spools_core_list(move |filter| {
            let mut view_model_borrow = moved_view_model.borrow_mut();

            // separated to not borrow twice
            let user_cores_changed = view_model_borrow.app_config.borrow().user_cores_changed_by_web_config;
            let spools_cores_filter = &view_model_borrow.spools_cores_filter;

            if user_cores_changed || spools_cores_filter != filter.as_str() {
                view_model_borrow.regenerate_cores_weights_list(filter.as_str());
                view_model_borrow.spools_cores_filter = filter.to_string();
                view_model_borrow.app_config.borrow_mut().user_cores_changed_by_web_config = false;
            }
            view_model_borrow.cores_list_vec_rc.clone()
        });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        ui_app_backend.on_get_spool_core_weight(move |id| *moved_view_model.borrow().spools_cores_weights.get(&id).unwrap_or(&0));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        ui_app_backend.on_erase_previously_used_core_list(move || moved_view_model.borrow_mut().erase_previously_used_cores_list());

        let spools_core_filter = self.spools_cores_filter.clone();
        self.regenerate_cores_weights_list(&spools_core_filter);
    }

    pub fn regenerate_cores_weights_list(&mut self, filter: &str) {
        // Fill spool cores weights list

        self.spools_cores_weights.clear();

        let cores_list_vec = self
            .cores_list_vec_rc
            .as_any()
            .downcast_ref::<slint::VecModel<crate::app::SelectorOption>>()
            .unwrap();
        cores_list_vec.clear();

        let mut id = -1;
        let app_config_clone = self.app_config.clone();
        if let Some(user_cores) = &app_config_clone.borrow().user_cores {
            id = self.add_core_weights_csv_to_list(id, user_cores.as_str(), "My Spools List", filter);
        }
        if let Some(previously_used_cores) = &app_config_clone.borrow().previously_used_cores {
            id = self.add_core_weights_csv_to_list(id, previously_used_cores.as_str(), "Previously Used", filter);
        }
        let _id = self.add_core_weights_csv_to_list(id, SPOOLS_CATALOG, "SpoolEase Spools Catalog", filter);
    }

    pub fn add_to_previously_used_cores(&mut self, core_name: &str, core_weight: i32) {
        if core_name.is_empty() {
            return;
        }
        let line_start = format!("{core_name},"); // the ',' is important, because one name could include another
        let mut app_config_borrow = self.app_config.borrow_mut();
        let mut new_previously_used_cores;
        if let Some(user_cores) = &app_config_borrow.user_cores {
            let line_found = user_cores.lines().find(|line| line.starts_with(&line_start));
            if line_found.is_some() {
                return;
            }
        }
        if let Some(previously_used_cores) = &app_config_borrow.previously_used_cores {
            let line_found = previously_used_cores.lines().enumerate().find(|line| line.1.starts_with(&line_start));
            if let Some((index, line)) = line_found {
                if index == 0 {
                    return;
                } else {
                    let line_to_remove = format!("{line}\r\n");
                    new_previously_used_cores = previously_used_cores.replace(&line_to_remove, "");
                    new_previously_used_cores.insert_str(0, &format!("{core_name},{core_weight}\r\n"));
                }
            } else {
                new_previously_used_cores = format!("{core_name},{core_weight}\r\n{previously_used_cores}");
            }
        } else {
            new_previously_used_cores = format!("{core_name},{core_weight}\r\n");
        }

        // limit to 9 previously used
        if new_previously_used_cores.lines().count() > 9 {
            if let Some(last_crlf) = new_previously_used_cores.rfind("\r\n") {
                if let Some(last_2nd_crlf) = new_previously_used_cores[..last_crlf].rfind("\r\n") {
                    new_previously_used_cores = new_previously_used_cores[..last_2nd_crlf + 2].to_string();
                }
            }
        }

        let _ = app_config_borrow.set_previously_used_cores(Some(new_previously_used_cores));
        drop(app_config_borrow);
        let spools_cores_filter = self.spools_cores_filter.clone();
        self.regenerate_cores_weights_list(&spools_cores_filter);
    }
    pub fn erase_previously_used_cores_list(&mut self) {
        let _ = self.app_config.borrow_mut().set_previously_used_cores(None);
        let spools_cores_filter = self.spools_cores_filter.clone();
        self.regenerate_cores_weights_list(&spools_cores_filter);
    }

    pub fn add_core_weights_csv_to_list(&mut self, last_id: i32, csv: &str, title: &str, filter: &str) -> i32 {
        // returns last-id used

        let cores_list = self
            .cores_list_vec_rc
            .as_any()
            .downcast_ref::<slint::VecModel<crate::app::SelectorOption>>()
            .unwrap();
        let mut selector_option = crate::app::SelectorOption::default();
        let mut id = last_id;
        selector_option.id = -1;
        selector_option.text = title.into();
        cores_list.push(selector_option);

        csv.lines().for_each(|line| {
            let mut split = line.splitn(4, ',');
            loop {
                if let (Some(desc), Some(weight)) = (split.next(), split.next()) {
                    if !desc.is_empty() && !weight.is_empty() && (filter.is_empty() || desc.to_uppercase().contains(filter)) {
                        id += 1;
                        let mut selector_option = crate::app::SelectorOption::default();
                        selector_option.id = id as i32;
                        selector_option.text = desc.trim().into();
                        cores_list.push(selector_option);
                        if let Ok(weight) = weight.trim().parse() {
                            self.spools_cores_weights.insert(id, weight);
                        } else {
                            error!("Error in Spool Line: '{line}'");
                        }
                    }
                } else {
                    break;
                }
            }
        });
        id
    }

    fn perform_select_printer(
        moved_ui: slint::Weak<crate::app::AppWindow>,
        moved_view_model: Rc<RefCell<ViewModel>>,
        selected_printer: &SharedString,
    ) {
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

    pub fn web_app_set_encode_info(&self, encode_info: &EncodeInfoDTO) {
        let ui = self.ui_weak.unwrap();

        // Initialize UI FrameworkState with framework information
        let ui_app_state = ui.global::<crate::app::AppState>();
        let mut encode_request = ui_app_state.get_curr_encode_request();
        encode_request.brand = encode_info.brand.to_shared_string();
        encode_request.color_name = encode_info.color_name.to_shared_string();
        encode_request.filament_subtype = encode_info.filament_subtype.to_shared_string();
        encode_request.note = encode_info.note.to_shared_string();
        ui_app_state.set_curr_encode_request(encode_request);
    }

    pub fn web_app_get_encode_info(&self) -> EncodeInfoDTO {
        let ui = self.ui_weak.unwrap();

        // Initialize UI FrameworkState with framework information
        let ui_app_state = ui.global::<crate::app::AppState>();
        let encode_request = ui_app_state.get_curr_encode_request();
        EncodeInfoDTO {
            brand: encode_request.brand.into(),
            color_name: encode_request.color_name.into(),
            filament_subtype: encode_request.filament_subtype.into(),
            note: encode_request.note.into(),
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
        let moved_view_model = self.view_model.clone().unwrap();
        moved_ui.unwrap().global::<crate::app::AppBackend>().on_encode_tag(move |encode_request| {
            info!("Request to encode tag with tray {} info", encode_request.tray_id);
            // Start with adding the core info to the previoysly used list
            if !encode_request.core_name.is_empty() {
                moved_view_model
                    .borrow_mut()
                    .add_to_previously_used_cores(encode_request.core_name.as_str(), encode_request.weight_core);
            }

            // Continue to encode
            let spool_tag = moved_spool_tag.borrow();
            let tray_id = usize::try_from(encode_request.tray_id).unwrap();
            let borrowed_filament_staging = moved_filament_staging.borrow();
            let mut tag_info_to_encode = if tray_id == 999 {
                // Encode from Staging
                if let Some(staging_tag_info) = borrowed_filament_staging.tag_info.clone() {
                    staging_tag_info
                } else {
                    return 0; // signals an error, UI will not continue
                }
            } else {
                match moved_bambu_printer.borrow().get_tag_info_to_encode(tray_id) {
                    Ok(tag_info) => tag_info,
                    Err(err) => {
                        // hopefully no borrowing issues since calling into ui in a callback
                        moved_ui
                            .unwrap()
                            .global::<crate::app::AppState>()
                            .invoke_encoding_failed(err.to_shared_string());
                        return 0; // signals an error, UI will not continue
                    }
                }
            };
            // let spool_scale_weight = moved_spool_scale.borrow().weight;
            tag_info_to_encode.weight_new = (encode_request.weight_new != 0).then(|| encode_request.weight_new);
            tag_info_to_encode.weight_core = (encode_request.weight_core != 0).then(|| encode_request.weight_core);
            tag_info_to_encode.brand = (!encode_request.brand.trim().is_empty()).then(|| encode_request.brand.trim().to_string());
            tag_info_to_encode.filament_subtype =
                (!encode_request.filament_subtype.trim().is_empty()).then(|| encode_request.filament_subtype.trim().to_string());
            tag_info_to_encode.color_name = (!encode_request.color_name.trim().is_empty()).then(|| encode_request.color_name.trim().to_string());
            tag_info_to_encode.note = (!encode_request.note.trim().is_empty()).then(|| encode_request.note.trim().to_string());

            let bambu_printer_borrow = moved_bambu_printer.borrow();
            if let Some(descriptor) =
                &tag_info_to_encode.to_descriptor(&bambu_printer_borrow.printer_name, &bambu_printer_borrow.printer_uuid_to_encode)
            {
                spool_tag.write_tag(&descriptor, tray_id);
            }
            info!("Sent the write request of tray {}", tray_id);
            // TODO: Get proper timeout fron config and pass it in the write_tag to spool_tag
            15
        });

        // handler for request from UI to reset printer, should work only on selected printer
        let moved_bambu_printer = self.bambu_printer_model.clone();
        let moved_ui = self.ui_weak.clone();
        self.ui_weak.unwrap().global::<crate::app::AppBackend>().on_reset_printer(move || {
            moved_bambu_printer.borrow_mut().reset_printer();
            moved_ui.unwrap().global::<crate::app::AppState>().invoke_reset_printer();
        });

        // handle encoding related listener(s) - this depends on current printer
        let moved_ui = self.ui_weak.clone();
        let moved_staging = self.filament_staging.clone();
        let moved_bambu = self.bambu_printer_model.clone();
        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_calc_encode_request_display(move || {
                let moved_ui = moved_ui.unwrap();
                let ui_app_state = moved_ui.global::<crate::app::AppState>();
                let mut encode_request = ui_app_state.get_curr_encode_request();
                let mut encode_request_display = ui_app_state.get_curr_encode_request_display();
                encode_request_display.pa_line1 = "".into();
                encode_request_display.pa_line2 = "".into();
                let tray_id = encode_request.tray_id;

                let staging_borrow = moved_staging.borrow();
                let bambu_borrow = moved_bambu.borrow();
                let (filament_info, tag_info) = match tray_id {
                    999 => { // Staging
                        if let Some(tag_info) = &staging_borrow.tag_info {
                            if let Some(filament_info) = &tag_info.filament {
                                (Some(filament_info.clone()), &staging_borrow.tag_info)
                            } else {
                                (None, &None)
                            }
                        } else {
                            (None, &None)
                        }
                    }
                    254 => { // External Tray
                        let tray = &bambu_borrow.virt_tray;
                        if let Some(calibration) = bambu_borrow.get_tray_calibration(&tray) {
                            encode_request_display.pa_line2 = format!("{}, {}", calibration.k_value, calibration.name,).into();
                        }
                        if let bambu::Filament::Known(filament_info) = &tray.filament {
                            (Some(filament_info.clone()), &tray.tag_info)
                        } else {
                            (None, &None)
                        }
                    }
                    0..15 => { // Standard trays
                        // let bambu = moved_bambu.borrow();
                        let tray = &bambu_borrow.ams_trays[tray_id as usize];
                        if let Some(calibration) = bambu_borrow.get_tray_calibration(&tray) {
                            encode_request_display.pa_line2 = format!("{}, {}", calibration.k_value, calibration.name,).into();
                        }
                        if let bambu::Filament::Known(filament_info) = &tray.filament {
                            (Some(filament_info.clone()), &tray.tag_info)
                        } else {
                            (None, &None)
                        }
                    }
                    _ => {
                        error!("UI request to update display for tray out of range, software error or pringer issue");
                        (None, &None)
                    }
                };

                if let Some(filament_info) = filament_info {
                    let first_request_to_display = encode_request_display.filament_type.is_empty(); // checking tray_type is empty to know if it is the first time, later if user changes to empty some value it shouldn't be overriden
                    if first_request_to_display {
                        if let Some(tag_info) = tag_info {
                            if encode_request.brand.is_empty() && tag_info.brand.is_some() {
                                encode_request.brand = tag_info.brand.as_ref().unwrap().to_shared_string();
                            }
                            if encode_request.filament_subtype.is_empty() && tag_info.filament_subtype.is_some() {
                                encode_request.filament_subtype = tag_info.filament_subtype.as_ref().unwrap().to_shared_string();
                            }
                            if encode_request.color_name.is_empty() && tag_info.color_name.is_some() {
                                encode_request.color_name = tag_info.color_name.as_ref().unwrap().to_shared_string();
                            }
                            if encode_request.note.is_empty() && tag_info.note.is_some() {
                                encode_request.note = tag_info.note.as_ref().unwrap().to_shared_string();
                            }
                        }
                    }

                    encode_request_display.color_code = filament_info.tray_color[..filament_info.tray_color.len().min(6)].into();
                    encode_request_display.temp_min = filament_info.nozzle_temp_min.try_into().unwrap_or_default();
                    encode_request_display.temp_max = filament_info.nozzle_temp_max.try_into().unwrap_or_default();
                    encode_request_display.filament_type = filament_info.tray_type.into();
                    // TODO: Add support for custom filaments here

                    let mut found_filament_name = false;
                    for line in BASE_FILAMENTS.lines() {
                        if let Some((code, name)) = line.split_once(',') {
                            if code == filament_info.tray_info_idx {
                                encode_request_display.slicer_name = format!("{name} (Base)").into();
                                found_filament_name = true;
                                break;
                            }
                        }
                    }
                    if !found_filament_name {
                        let view_model_borrow = moved_view_model.borrow();
                        let app_config_borrow = view_model_borrow.app_config.borrow();
                        if let Some(custom_filaments) = &app_config_borrow.custom_filaments {
                            for line in custom_filaments.lines() {
                                if let Some((code, name)) = line.split_once(',') {
                                    if code == filament_info.tray_info_idx {
                                        encode_request_display.slicer_name = format!("{name} (Custom)").into();
                                        found_filament_name = true;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if !found_filament_name {
                        encode_request_display.slicer_name = "Unknown Filament".into();
                    }
                    if !encode_request_display.pa_line2.is_empty() {
                        encode_request_display.pa_line1 = format!(
                            "{}, {}",
                            moved_bambu.borrow().printer_name,
                            moved_bambu.borrow().nozzle_diameter.as_ref().unwrap_or(&"Unknown".to_string())
                        )
                        .into();
                    }
                    ui_app_state.set_curr_encode_request_display(encode_request_display);
                    if first_request_to_display {
                        ui_app_state.set_curr_encode_request(encode_request);
                    }
                }
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
            weight_core: tag_info.weight_core.unwrap_or_default(),
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

        if bambu_printer.printer_index == current_selected_printer {
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
            // if bambu_printer.printer_number == 1 { // UNREMARK FOR TESTS WITH ONE PRINTER
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
            self.ui_weak
                .unwrap()
                .global::<crate::app::AppState>()
                .invoke_printer_connected(bambu_printer.printer_selector_name.to_shared_string());
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

    fn on_pn532_status(&mut self, status: bool) {
        self.app_config.borrow_mut().report_pn532(status);
    }

    fn on_emulated_tag_read(&mut self) {
        info!("Emulated tag scanned");
        let ui = self.ui_weak.clone();
        ui.unwrap().global::<crate::app::AppState>().invoke_emulated_tag_scanned();
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
        if newer {
            info!("OTA: New version {version}");
        } else {
            info!("OTA: Up to date with available version {version}");
        }
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_set_ota_info(crate::app::OtaInfo {
                version: version.to_shared_string(),
                newer,
            });
    }

    fn on_webapp_url_update(&self, ip_url: &str, name_url: Option<&str>, ssid: &str) {
        let final_url = if let Some(name_url) = name_url {
            &format!("{ip_url} / {name_url}")
        } else {
            ip_url
        };
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_set_web_config_url(final_url.to_shared_string(), SharedString::from(ssid));
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

impl SpoolScaleObserver for ViewModel {
    fn on_scale_loaded(&mut self, weight: i32) {
        info!("Scale loaded with {weight} g");
        self.framework.borrow().undim_display();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppState>()
            .invoke_spool_scale_loaded(weight, false);
    }

    fn on_scale_load_changed_stable(&mut self, weight: i32) {
        debug!("Scale load changed to stable {weight}");
        self.framework.borrow().undim_display();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppState>()
            .invoke_spool_scale_load_changed(weight, true);
    }

    fn on_scale_load_changed_unstable(&mut self, weight: i32) {
        debug!("Scale load changed to unstable {weight}");
        self.framework.borrow().undim_display();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppState>()
            .invoke_spool_scale_load_changed(weight, false);
    }

    fn on_scale_load_removed(&mut self) {
        debug!("Scale load removed");
        self.framework.borrow().undim_display();
        self.ui_weak.unwrap().global::<crate::app::AppState>().invoke_spool_scale_load_removed();
    }

    fn on_scale_raw_samples_avg(&mut self, raw_data: i32) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppState>()
            .invoke_spool_scale_raw_samples_avg(raw_data);
    }

    fn on_scale_connected(&mut self) {
        debug!("Scale connected");
        self.ui_weak.unwrap().global::<crate::app::AppState>().invoke_spool_scale_connected();
    }

    fn on_scale_disconnected(&mut self) {
        debug!("Scale disconnected");
        self.ui_weak.unwrap().global::<crate::app::AppState>().invoke_spool_scale_disconnected();
    }

    fn on_scale_uncalibrated(&mut self) {
        debug!("Scale uncalibrated");
        self.ui_weak.unwrap().global::<crate::app::AppState>().invoke_spool_scale_uncalibrated();
    }

    fn on_term_text(&mut self, text: &str) {
        let text = format!("\n[S] {text}");
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_add_term_text(text.into());
    }

    fn on_tag_status(&mut self, status: &shared::spool_tag::Status) {
        SpoolTagObserver::on_tag_status(self, status);
    }

    fn on_pn532_status(&mut self, status: bool) {
        if status {
            term_info!("[S] Scale initialized the NFC module successfuly");
        } else {
            term_info!("[S] Warning: Scale failed to initialize the NFC module");
        }
    }
}
