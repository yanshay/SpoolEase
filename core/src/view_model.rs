use core::cell::RefCell;
use core::cmp::max;
use core::ops::{Deref, DerefMut};

use alloc::boxed::Box;
use alloc::string::String;
use alloc::{
    format,
    rc::{Rc, Weak},
    string::ToString,
    vec::Vec,
};
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{with_timeout, Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use hashbrown::HashMap;
use ndef_rs::payload::UriPayload;
use ndef_rs::NdefMessage;
use serde::{Deserialize, Serialize};
use shared::gcode_analysis_task::{
    fetch_gcode_analysis_task, Fetch3mf, FilamentUsage, GcodeAnalysisNotification, GcodeAnalysisNotificationChannel, GcodeAnalysisRequest,
    GcodeAnalysisRequestChannel, GcodeAnalyzerObserver,
};
use shared::settings::{
    OTA_DOMAIN_DEBUG, OTA_DOMAIN_STABLE, OTA_DOMAIN_UNSTABLE, OTA_TLS_CERTIFICATE, SCALE_DEBUG_OTA_PATH, SCALE_STABLE_OTA_PATH, SCALE_UNSTABLE_OTA_PATH
};
use shared::types::AppOtaTrain;
use shared::utils::channel_send;
use slint::{ComponentHandle, Model, SharedString, ToSharedString};

use framework::prelude::*;
use framework::{
    framework::{FrameworkObserver, WebConfigMode},
    terminal::{self, term_mut, TerminalObserver},
};

use crate::app::{UiSlotDisplay, UiSpoolRecord, UiSpoolRecordDisplay};
use crate::app_config::{BASE_FILAMENTS, FILAMENT_BRAND_NAMES, MATERIALS};
use crate::app_ota::{app_ota_task, AppOtaProduct, AppOtaRequest, AppOtaRequestChannel};
use crate::bambu::bambu_print::PrintProject;
use crate::bambu::{Filament, KExtruder, KInfo, KNozzleDiameter, KNozzleId, KPrinter, SpoolId, Tray, TrayBits};
use crate::color_utils::get_color_name;
use crate::filament_staging::StagingOrigin;
use crate::settings::OTA_TOML_FILENAME;
use crate::spool_record::{FullSpoolRecord, OriginData, SpoolRecord, SpoolRecordExt};
use crate::spool_scale::{self, ScaleWeight, SpoolScaleObserver};
use crate::ssdp::{ssdp_task, SSDPPubSubChannel};
use crate::store::{store_safe_time_now, Store, StoreObserver};

use crate::tag_standards::{BambuLabTag, OpenPrintTagTag, BAMBULAB_TAG_TYPE, OPENPRINTTAG_TAG_TYPE};
use crate::types::FilamentSupInfo;
use crate::{
    app_config::AppConfig,
    bambu::{self, BambuPrinter, BambuPrinterObserver, TagInformationV1, TrayState},
    filament_staging::FilamentStaging,
};
use shared::spool_tag::{self, SpoolTagObserver, Status, TAG_PLACEHOLDER};

#[allow(dead_code)]
const EXTRA_DEBUG: bool = false;

#[allow(unused_macros)]
macro_rules! debugex {
    ($($t:tt)*) => {
        if EXTRA_DEBUG {
            debug!($($t)*);
        }
    };
}

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
    pub bambu_printer_model: SelectedPrinter,
    spool_tag_model: Rc<RefCell<spool_tag::SpoolTag>>,
    spool_scale_model: Rc<RefCell<spool_scale::SpoolScale>>,
    pub filament_staging: Rc<RefCell<FilamentStaging>>,
    printers_view_state: HashMap<String, PrinterUiState>,

    // cores_list_vec_rc: slint::ModelRc<crate::app::SelectorOption>,
    // spools_cores_weights: HashMap<i32, i32>,
    // spools_cores_filter: String,
    pub store: Rc<Store>,
    gcode_analysis_request_channel: Rc<GcodeAnalysisRequestChannel>,
    gcode_analysis_notification_channel: Rc<GcodeAnalysisNotificationChannel>,
    gcode_last_job_number: i32,
    gcode_jobs: Vec<GcodeJob>,
    console_available_gcode_tasks: usize,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
    app_async_tasks_channel: Rc<AppAsyncTasksChannel>,
    pub recently_added_spool_id: Option<String>,
    store_state_request_channel: Rc<StoreStateRequestChannel>,
    pub app_ota_request_channel: Rc<AppOtaRequestChannel>,
    pub scale_version: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SpoolEncodeCookie {
    spool_rec_id: String,
    encode_time: Option<i32>,
}

#[derive(Serialize, Deserialize, Clone)]
struct LocationEncodeCookie {
    location: String, // required for now just to identify the write was for a location tag
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
        let terminal_view_model = Rc::new(RefCell::new(TerminalViewModel {
            ui_weak: ui_weak.clone(),
            term_text: String::with_capacity(8192),
        }));
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
        spawner.spawn_heap(ssdp_task(framework.clone(), ssdp_pub_sub)).ok();

        // Initialize store
        let store = Store::new(framework.clone());

        // Initialize spool_scale_model
        let spool_scale_model = crate::spool_scale::init(framework.clone(), app_config.clone(), stack, spawner, ssdp_pub_sub);

        // // Prepare an empty spool weights lists, later we'll replace it
        // let spools_cores_weights: HashMap<i32, i32> = HashMap::with_capacity(300);
        // let selector_options_vec: slint::VecModel<crate::app::SelectorOption> = slint::VecModel::default();
        // let selector_options_vec_rc = slint::ModelRc::from(Rc::new(selector_options_vec));

        let gcode_analysis_request_channel = Rc::new(GcodeAnalysisRequestChannel::new());
        let gcode_analysis_notification_channel = Rc::new(GcodeAnalysisNotificationChannel::new());
        let app_async_tasks_channel = Rc::new(AppAsyncTasksChannel::new());

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
            filament_staging: Rc::new(RefCell::new(FilamentStaging::new(store.clone()))),
            printers_view_state: HashMap::new(),
            // cores_list_vec_rc: selector_options_vec_rc,
            // spools_cores_weights,
            // spools_cores_filter: String::new(),
            store,
            gcode_analysis_request_channel,
            gcode_analysis_notification_channel,
            gcode_last_job_number: 0,
            gcode_jobs: Vec::new(),
            console_available_gcode_tasks: 0,
            ssdp_pub_sub,
            app_async_tasks_channel,
            recently_added_spool_id: None,
            store_state_request_channel: Rc::new(StoreStateRequestChannel::new()),
            app_ota_request_channel: Rc::new(AppOtaRequestChannel::new()),
            scale_version: None,
        };
        let view_model_rc = Rc::new(RefCell::new(view_model));

        // hold a reference to itself to hand over to others, this is a 'memory leak' but object never gets destroyed so eaiser than weak reference
        view_model_rc.borrow_mut().view_model = Some(view_model_rc.clone());

        // Initialize
        view_model_rc.borrow_mut().init_framework_stuff();
        view_model_rc.borrow_mut().init_app_stuff();

        // later from main will be called the part that depends on sd_card only if sd_card initialized properly

        // Done
        view_model_rc
    }

    pub fn message_box(&self, title: &str, text: &str, text2: &str, status_type: crate::app::StatusType, timeout: i32) {
        let ui = self.ui_weak.unwrap();
        let ui_app_state: crate::app::AppState<'_> = ui.global::<crate::app::AppState>();
        ui_app_state.invoke_show_message_box(title.into(), text.into(), text2.into(), status_type, timeout);
        self.framework.borrow().undim_display();
    }

    pub fn init_only_if_sdcard_init_ok(&mut self) {
        self.store.start(self.view_model.clone().unwrap());

        // Initialize Printers ///////////////////////////

        let mut default_printer_set = false;
        let mut printer_number = 1; // starts from one and incremented for any printer
        let mut printer_index = 0; // starts from zero and incremented only on successful init and adding to array
        let mut available_printers: Vec<SharedString> = Vec::new();
        for printer_config in &self.app_config.borrow().configured_printers.printers {
            if printer_number > 5 {
                term_info!("Printers limit reached - max five printers supported");
                break;
            }
            match bambu::init(
                self.framework.clone(),
                printer_number,
                printer_index,
                printer_config,
                self.app_config.clone(),
                self.ssdp_pub_sub,
                self.store_state_request_channel.clone(),
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

        let ui = self.ui_weak.unwrap();
        let ui_app_backend = ui.global::<crate::app::AppBackend>();
        let ui_app_state = ui.global::<crate::app::AppState>();

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
            self.framework
                .borrow()
                .spawner
                .spawn_heap(printers_scheduled_store_state_task(
                    self.framework.clone(),
                    self.view_model.clone().unwrap(),
                    self.store.clone(),
                ))
                .ok();

            self.framework
                .borrow()
                .spawner
                .spawn_heap(store_printers_consume(self.view_model.clone().unwrap()))
                .ok();
        }
        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_link_tag_to_spool_id(move |tag_id, tag_type, spool_id, final_step| {
            let _ = moved_view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::LinkTagToSpool {
                tag_id: tag_id.into(),
                tag_type: tag_type.into(),
                spool_id: spool_id.into(),
                final_step,
            });
        });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_unlink_spool_from_tag(move |spool_id| {
            let _ = moved_view_model
                .borrow()
                .dispatch_async_task(AppAsyncTaskRequest::UnLinkSpoolFromTag { spool_id: spool_id.into() });
        });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_set_spool_weight(move |spool_id, weight_current, weight_new, final_step| {
            let _ = moved_view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::SetSpoolWeight {
                spool_id: spool_id.into(),
                weight_current,
                weight_new,
                final_step,
                from_button: false,
            });
        });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_recently_added_spool_id_if_untagged(move || {
            let store = moved_view_model.borrow().store.clone();
            if let Some(spool_id) = &moved_view_model.borrow().recently_added_spool_id {
                if let Some(spool_rec) = store.get_spool_by_id(spool_id) {
                    if spool_rec.tag_id.is_empty() {
                        return spool_id.to_shared_string();
                    }
                }
            }
            SharedString::new()
        });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_encode_tag(move || {
            let view_model_borrow = moved_view_model.borrow();
            let filament_staging_borrow = view_model_borrow.filament_staging.borrow();
            let ui_borrow = view_model_borrow.ui_weak.unwrap();
            let ui = ui_borrow.global::<crate::app::AppState>();
            match filament_staging_borrow.spool_rec() {
                Some(spool_rec) => {
                    let store = view_model_borrow.store.clone();
                    // getting most updated spool_rec from store (not from staging in case changed)
                    let mut spool_rec = if let Some(spool_rec) = store.get_spool_by_id(&spool_rec.id) {
                        spool_rec
                    } else {
                        ui.invoke_encoding_failure(slint::format!("Spool {} not Found", spool_rec.id));
                        return false;
                    };
                    spool_rec.encode_time = store_safe_time_now();
                    let filament_sup_info = view_model_borrow.get_filament_info(&spool_rec.slicer_filament, None);
                    match spool_rec.to_tag_descriptor_v2(&filament_sup_info) {
                        Some(descriptor) => {
                            let spool_tag_borrow = view_model_borrow.spool_tag_model.borrow();
                            let spool_scale_borrow = view_model_borrow.spool_scale_model.borrow();
                            let encode_cookie = SpoolEncodeCookie {
                                spool_rec_id: spool_rec.id,
                                encode_time: spool_rec.encode_time,
                            };
                            let encode_cookie_str = serde_json::to_string(&encode_cookie).unwrap();
                            if let Ok(uid) = hex::decode(spool_rec.tag_id) {
                                spool_tag_borrow.write_tag(&descriptor, Some(uid.clone()), encode_cookie_str.clone());
                                let _ = spool_scale_borrow.write_tag(&descriptor, Some(uid), encode_cookie_str);
                                true
                            } else {
                                ui.invoke_encoding_failure("Spool Tag Id isn't valid".to_shared_string());
                                false
                            }
                        }
                        None => {
                            ui.invoke_encoding_failure("Failed to Create Tag Descriptor".to_shared_string());
                            false
                        }
                    }
                }
                None => {
                    ui.invoke_encoding_failure("Staging is Empty".to_shared_string());
                    false
                }
            }
        });
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

        // not the OTA used any longer
        // let framework = self.framework.clone();
        // ui_framework_backend.on_update_firmware_ota(move || {
        //     framework.borrow().update_firmware_ota();
        // });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_info(move |text| moved_view_model.borrow().ui_info(text.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_debug(move |text| moved_view_model.borrow().ui_debug(text.as_str()));
    }

    pub fn init_app_stuff(&mut self) {
        self.framework
            .borrow()
            .spawner
            .spawn_heap(app_async_task(self.view_model.clone().unwrap()))
            .ok();

        self.framework
            .borrow()
            .spawner
            .spawn_heap(app_ota_task(self.framework.clone(), self.view_model.clone().unwrap()))
            .ok();

        // Subscribe to rust spool_tag events
        let trait_for_spool_tag_rc: Rc<RefCell<dyn spool_tag::SpoolTagObserver>> = self.view_model.as_ref().unwrap().clone();
        let trait_for_spool_tag_weak: Weak<RefCell<dyn spool_tag::SpoolTagObserver>> = Rc::downgrade(&trait_for_spool_tag_rc);
        self.spool_tag_model.borrow_mut().subscribe(trait_for_spool_tag_weak);

        // Subscribe to rust spool_scale events
        let trait_for_spool_scale_rc: Rc<RefCell<dyn spool_scale::SpoolScaleObserver>> = self.view_model.as_ref().unwrap().clone();
        let trait_for_spool_scale_weak: Weak<RefCell<dyn spool_scale::SpoolScaleObserver>> = Rc::downgrade(&trait_for_spool_scale_rc);
        self.spool_scale_model.borrow_mut().subscribe(trait_for_spool_scale_weak);

        // Subscribe to rust store events
        // It's a bit different because store is Rc<Store> and not Rc<RefCell<Store>> due to Store different needs
        // ...I already don't remember those needs ... maybe not really needed anymore and originated in trying to solve something else there
        let trait_for_store_rc: Rc<RefCell<dyn StoreObserver>> = self.view_model.as_ref().unwrap().clone();
        let trait_for_store_weak: Weak<RefCell<dyn StoreObserver>> = Rc::downgrade(&trait_for_store_rc);
        self.store.subscribe(trait_for_store_weak);

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
        let moved_spool_scale = self.spool_scale_model.clone();
        ui_app_backend.on_read_tag_mode(move || {
            moved_spool_tag.borrow().read_tag();
            if let Err(err) = moved_spool_scale.borrow().read_tag() {
                error!("Error sending read_tag to scale : {err}");
            }
        });

        let moved_spool_tag = self.spool_tag_model.clone();
        let moved_framework = self.framework.clone();
        let moved_app_config = self.app_config.clone();
        ui_app_backend.on_web_config_web_app(move || {
            moved_app_config.borrow_mut().set_redirect_web_to_config();
            let borrowed_framework = moved_framework.borrow();
            let web_config_ip_url = &borrowed_framework.web_config_ip_url;
            let web_config_key = &borrowed_framework.web_config_key;
            let full_web_config_url = format!("{web_config_ip_url}#sk={web_config_key}");
            moved_spool_tag.borrow().emulate_tag(&full_web_config_url);
        });

        // Spool Scale
        let scale_available = if let Some(scale_config) = &self.app_config.borrow().configured_scale {
            scale_config.available
        } else {
            false
        };
        if !scale_available {
            ui_app_state.set_spool_scale_state(crate::app::SpoolScaleState::NotAvailable);
        }
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

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_get_spool_record_display(move |spool_id| moved_view_model.borrow().ui_get_spool_record_display(&spool_id));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_get_spool_tag_definition_display(move |tag_definition_type, tag_definition_info, empty_spool_weight| {
                moved_view_model
                    .borrow()
                    .ui_get_spool_tag_definition_display(&tag_definition_type, &tag_definition_info, empty_spool_weight)
            });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_get_slot_display(move |tray_id| moved_view_model.borrow().ui_get_slot_display(tray_id));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_can_link_spool_to_tag(move |spool_id| moved_view_model.borrow().ui_can_link_spool_to_tag(spool_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_add_v1_tag_to_inventory(move |tag_id, tag, final_step| {
                moved_view_model
                    .borrow()
                    .ui_add_v1_tag_to_inventory(tag_id.as_str(), tag.as_str(), final_step)
            });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_import_definition_tag_to_inventory(move |tag_definition_type, tag_definition_info, empty_spool_weight| {
            moved_view_model.borrow().ui_import_definition_tag_to_inventory(
                tag_definition_type.as_str(),
                tag_definition_info.as_str(),
                empty_spool_weight,
            )
        });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_erase_tag(move |tag_id| moved_view_model.borrow().ui_erase_tag(tag_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_erase_tag_by_spool_id(move |spool_id| moved_view_model.borrow().ui_erase_tag_by_spool_id(spool_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_term_info(move |text| moved_view_model.borrow().ui_term_info(text.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_untag_slot(move |tray_id| moved_view_model.borrow().ui_untag_slot(tray_id));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_reset_slot(move |tray_id| moved_view_model.borrow().ui_reset_slot(tray_id));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_configure_slot_with_spool_id(move |tray_id, spool_id| moved_view_model.borrow().ui_configure_slot_with_spool_id(tray_id, &spool_id));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_ota_check_firmwares(move || moved_view_model.borrow().ui_ota_check_firmwares());

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_ota_update_firmware(move |product, train| moved_view_model.borrow().ui_ota_update_firmware(&product, &train));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_encode_location_tag(move || moved_view_model.borrow().ui_encode_location_tag());

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
            if selected_printer_string == printer.borrow().printer_selector_name {
                moved_ui
                    .unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_set_curr_printer(selected_printer.to_shared_string());
                borrowed_view_model.bambu_printer_model.index = i;

                // while strange, this is importnat here for restoring curr_ams after, next call will set it to the first (in case 0 doesn't exist)
                // Internally this will pass only for the currently displayed printer in the UI
                moved_ui.unwrap().global::<crate::app::AppState>().set_curr_ams_id(0);

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

    fn get_filament_info(&self, search_code: &str, material: Option<&str>) -> Option<FilamentSupInfo> {
        let app_config_borrow = self.app_config.borrow();
        let empty_list = String::new();
        let filament_lists = [BASE_FILAMENTS, app_config_borrow.custom_filaments.as_ref().unwrap_or(&empty_list)];

        let mut base = true;
        for filament_list in filament_lists {
            for line in filament_list.lines() {
                let mut split = line.split(',');
                if let (Some(code), Some(name), Some(nozzle_temp_low), Some(nozzle_temp_high)) =
                    (split.next(), split.next(), split.next(), split.next())
                {
                    if code == search_code {
                        let name = decode_csv_field(name);
                        let nozzle_temp_low = nozzle_temp_low.parse::<i32>().unwrap_or_default();
                        let nozzle_temp_high = nozzle_temp_high.parse::<i32>().unwrap_or_default();
                        return Some(FilamentSupInfo {
                            origin_is_material: false,
                            base_filament: base,
                            slicer_name: name,
                            slicer_code: code.to_string(),
                            nozzle_temp_low,
                            nozzle_temp_high,
                        });
                    }
                }
            }
            base = false;
        }
        // here it means not found the slicer filament, so resorting to material type

        if let Some(material) = material {
            let mut material_code = "";
            let mut found = false;
            for (line_index, material_line) in MATERIALS.lines().enumerate() {
                if line_index == 0 {
                    continue;
                } // skip title line
                let mut split = material_line.split(',');
                if let Some(list_material) = split.next() {
                    if list_material == material {
                        if let (Some(filament_id), Some(nozzle_temp_low), Some(nozzle_temp_high)) = (split.next(), split.next(), split.next()) {
                            if let (Ok(_wrong_nozzle_temp_low), Ok(_wrong_nozzle_temp_high)) =
                                (nozzle_temp_low.parse::<u32>(), nozzle_temp_high.parse::<u32>())
                            {
                                material_code = filament_id;
                                found = true;
                                break;
                            }
                        }
                    }
                }
            }

            if found {
                for line in BASE_FILAMENTS.lines() {
                    let mut split = line.split(',');
                    if let (Some(code), Some(name), Some(nozzle_temp_low), Some(nozzle_temp_high)) =
                        (split.next(), split.next(), split.next(), split.next())
                    {
                        if code == material_code {
                            let name = decode_csv_field(name);
                            let nozzle_temp_low = nozzle_temp_low.parse::<i32>().unwrap_or_default();
                            let nozzle_temp_high = nozzle_temp_high.parse::<i32>().unwrap_or_default();
                            return Some(FilamentSupInfo {
                                origin_is_material: true,
                                base_filament: true,
                                slicer_name: name,
                                slicer_code: code.to_string(),
                                nozzle_temp_low,
                                nozzle_temp_high,
                            });
                        }
                    }
                }
            }
        }

        None
    }

    fn set_staging_to_tray_direct(
        &self,
        filament_staging: &Rc<RefCell<FilamentStaging>>,
        bambu_printer: &mut BambuPrinter,
        ui: &slint::Weak<crate::app::AppWindow>,
        tray_id: i32,
    ) {
        let tray_id_for_ui = tray_id;
        let full_slot_description = Self::full_slot_description(bambu_printer, tray_id);
        let mut filament_staging = filament_staging.borrow_mut();
        if bambu_printer.printer_connectivity_ok != Some(true) {
            ui.unwrap().global::<crate::app::AppState>().invoke_tray_update_failed(
                bambu_printer.printer_selector_name.to_shared_string(),
                full_slot_description.into(),
                "Printer disconnected".to_shared_string(),
            );
        } else if let Some(full_spool_rec) = filament_staging.full_spool_rec() {
            if let Some(filament_info) =
                self.get_filament_info(&full_spool_rec.spool_rec.slicer_filament, Some(&full_spool_rec.spool_rec.material_type))
            {
                bambu_printer.set_tray_filament(
                    tray_id,
                    full_spool_rec,
                    filament_info.nozzle_temp_low as u32,
                    filament_info.nozzle_temp_high as u32,
                );
                filament_staging.clear();
                ui.unwrap().global::<crate::app::AppState>().invoke_empty_spool_staging();
                ui.unwrap().global::<crate::app::AppState>().invoke_tray_update_succeeded(
                    bambu_printer.printer_selector_name.to_shared_string(),
                    full_slot_description.into(),
                    tray_id_for_ui,
                );
            } else {
                ui.unwrap().global::<crate::app::AppState>().invoke_tray_update_failed(
                    bambu_printer.printer_selector_name.to_shared_string(),
                    full_slot_description.into(),
                    slint::format!("Spool {} Missing Required Information", full_spool_rec.spool_rec.id),
                );
            }
        }
    }

    fn full_slot_description(bambu_printer: &BambuPrinter, tray_id: i32) -> String {
        let (ams_id, slot_in_ams) = BambuPrinter::get_ams_and_slot_id(tray_id as usize);
        if ams_id <= 3 {
            format!("{} Slot {}", Self::ams_name(bambu_printer, ams_id), slot_in_ams + 1)
        } else {
            Self::ams_name(bambu_printer, ams_id)
        }
    }

    fn ams_name(bambu_printer: &BambuPrinter, ams_id: usize) -> String {
        if ams_id <= 3 {
            format!("AMS-{}", (b'A' + ams_id as u8) as char)
        } else if (128..128 + 8).contains(&ams_id) {
            format!("HT-{}", (b'A' + (ams_id - 128) as u8) as char)
        } else if ams_id == 255 {
            if bambu_printer.num_extruders() == 1 {
                "External Spool".into()
            } else {
                "Right External Spool".into()
            }
        } else if ams_id == 254 {
            "Left External Spool".into()
        } else {
            format!("AMS-#{ams_id}?")
        }
    }

    fn set_staging_to_tray(
        view_model: &Rc<RefCell<ViewModel>>,
        filament_staging: &Rc<RefCell<FilamentStaging>>,
        bambu_printer: &Rc<RefCell<BambuPrinter>>,
        ui: &slint::Weak<crate::app::AppWindow>,
        tray_id: i32,
    ) {
        view_model
            .borrow()
            .set_staging_to_tray_direct(filament_staging, &mut bambu_printer.borrow_mut(), ui, tray_id);
    }

    // TODO: check the neccessity of this function and if all content is relevant to it
    fn register_printer_related_listeners(&mut self) {
        // handler for request from UI to move to staging, need to work only on selected printer
        // this way of doing it (with many 'moved' might be due to being called when printer is already borrowed, or staging borrowed(less likely) )
        let moved_filament_staging = self.filament_staging.clone();
        let moved_bambu_printer = self.bambu_printer_model.clone();
        let moved_view_model = self.view_model.clone().unwrap();
        let moved_ui = self.ui_weak.clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_set_staging_to_tray(move |tray_id: i32| {
                Self::set_staging_to_tray(&moved_view_model, &moved_filament_staging, &moved_bambu_printer, &moved_ui, tray_id);
            });
    }

    fn ui_encode_location_tag(&self) {
        const LOCATION_URL_PREFIX_V1: &str = "https://tag.spoolease.io/L1/";
        let spool_tag_borrow = self.spool_tag_model.borrow();
        let descriptor = format!("{LOCATION_URL_PREFIX_V1}?TG={TAG_PLACEHOLDER}");
        let encode_cookie = LocationEncodeCookie {
            location: String::new()
        };
        let encode_cookie_str = serde_json::to_string(&encode_cookie).unwrap();
        spool_tag_borrow.write_tag(&descriptor, None, encode_cookie_str);
    }

    fn ui_ota_update_firmware(&self, product: &str, train: &str) {
        let train = match train {
            "stable" => AppOtaTrain::Stable,
            "unstable" => AppOtaTrain::Unstable,
            "debug" => AppOtaTrain::Debug,
            _ => {
                error!("Internal Error: unsupported train {train} in request to update");
                return;
            }
        };
        match product {
            "console" => {
                channel_send(
                    &self.app_ota_request_channel,
                    AppOtaRequest::Update {
                        product: AppOtaProduct::Console,
                        train,
                    },
                );
            }
            "scale" => {
                info!("Sending request to update firmware to Scale");

                let (ota_domain, ota_path) = match train {
                    AppOtaTrain::Stable => (OTA_DOMAIN_STABLE, SCALE_STABLE_OTA_PATH),
                    AppOtaTrain::Unstable => (OTA_DOMAIN_UNSTABLE, SCALE_UNSTABLE_OTA_PATH),
                    AppOtaTrain::Debug => (OTA_DOMAIN_DEBUG, SCALE_DEBUG_OTA_PATH),
                };

                let _ = self
                    .spool_scale_model
                    .borrow()
                    .update_firmware(ota_domain, ota_path, OTA_TOML_FILENAME, OTA_TLS_CERTIFICATE);
            }
            _ => {
                error!("Internal error, unsupported product to update");
            }
        }
    }

    fn ui_ota_check_firmwares(&self) {
        channel_send(&self.app_ota_request_channel, AppOtaRequest::CheckOta {});
    }

    fn ui_configure_slot_with_spool_id(&self, slot_id: i32, spool_id: &str) {
        let _ = self.dispatch_async_task(AppAsyncTaskRequest::ConfigureTrayWithSpool {
            tray_id: slot_id,
            spool_id: spool_id.to_string(),
        });
    }

    fn ui_untag_slot(&self, tray_id: i32) {
        self.bambu_printer_model
            .borrow_mut()
            .update_any_tray(tray_id as usize, |tray| tray.meta_info.spool_id = None);
        self.update_ui_from_printer(&self.bambu_printer_model.borrow());
    }
    fn ui_reset_slot(&self, tray_id: i32) {
        self.bambu_printer_model.borrow_mut().reset_tray(tray_id);
        self.update_ui_from_printer(&self.bambu_printer_model.borrow());
    }
    fn ui_term_info(&self, text: &str) {
        self._terminal_view_model.borrow_mut().on_add_text(text);
    }
    fn ui_info(&self, text: &str) {
        info!("{}", text);
    }
    fn ui_debug(&self, text: &str) {
        debug!("{}", text);
    }

    fn ui_erase_tag_by_spool_id(&self, spool_id: &str) -> bool {
        if let Some(spool_rec) = self.store.get_spool_by_id(spool_id) {
            if spool_rec.has_valid_tag_id() {
                self.ui_erase_tag(&spool_rec.tag_id);
                return true;
            }
        }
        error!("Received to erase spool's tag with invalid tag id");
        let ui = self.ui_weak.unwrap();
        let ui_app_state: crate::app::AppState<'_> = ui.global::<crate::app::AppState>();
        ui_app_state.invoke_show_message_box(
            "Erase Tag Notice".into(),
            slint::format!("No Tag-Id for Spool {}", spool_id),
            SharedString::new(),
            crate::app::StatusType::Error,
            -1,
        );
        false
    }

    fn ui_erase_tag(&self, tag_id: &str) -> bool {
        if let Ok(uid) = hex::decode(tag_id) {
            let spool_tag_borrow = self.spool_tag_model.borrow();
            spool_tag_borrow.erase_tag(Some(uid.clone()), String::new());
            let _ = self.spool_scale_model.borrow().erase_tag(Some(uid), String::new());
            true
        } else {
            // ui.invoke_encoding_failure("Spool Tag Id isn't valid".to_shared_string());
            false
        }
    }

    fn ui_add_v1_tag_to_inventory(&self, tag_id: &str, tag: &str, final_step: bool) {
        let _ = self.dispatch_async_task(AppAsyncTaskRequest::ProcessV1TagRead {
            tag_id: tag_id.to_string(),
            tag: tag.to_string(),
            final_step,
        });
    }

    fn ui_import_definition_tag_to_inventory(&self, tag_definition_type: &str, tag_definition_info: &str, empty_spool_weight: i32) {
        let _ = self.dispatch_async_task(AppAsyncTaskRequest::ImportDefinitionTagToInventory {
            tag_definition_type: tag_definition_type.to_string(),
            tag_definition_info: tag_definition_info.to_string(),
            empty_spool_weight,
        });
    }

    fn ui_can_link_spool_to_tag(&self, id: &str) -> SharedString {
        if let Some(spool_rec) = self.store.get_spool_by_id(id) {
            if spool_rec.tag_id.is_empty() || spool_rec.tag_id.starts_with("-") {
                SharedString::new()
            } else {
                SharedString::from("Spool Is Tagged")
            }
        } else {
            SharedString::from("Spool Not Found")
        }
    }

    fn ui_get_slot_display(&self, tray_id: i32) -> UiSlotDisplay {
        let printer_borrow = self.bambu_printer_model.borrow();
        let tray = printer_borrow.get_any_tray(tray_id as usize);
        let color_code = if let Filament::Known(filament) = &tray.filament {
            filament.tray_color.to_shared_string()
        } else {
            SharedString::new()
        };
        let (slicer_name, temp_min, temp_max, material) = if let Filament::Known(filament) = &tray.filament {
            if let Some(filament_info) = self.get_filament_info(&filament.tray_info_idx, Some(&filament.tray_type)) {
                (
                    slint::format!(
                        "{}{}",
                        filament_info.slicer_name,
                        if filament_info.base_filament { " (base)" } else { "" }
                    ),
                    filament_info.nozzle_temp_low,
                    filament_info.nozzle_temp_high,
                    filament.tray_type.as_str(),
                )
            } else {
                (SharedString::new(), 0, 0, filament.tray_type.as_str())
            }
        } else {
            (SharedString::new(), 0, 0, "")
        };

        let color_name = if color_code.len() >= 6 {
            let color = u32::from_str_radix(&color_code[..6], 16).unwrap() + 0xFF000000; // the plus 0xFF at the end is fo add alpha
            let color = slint::Color::from_argb_encoded(color);
            let color_name_info = get_color_name(color.red(), color.green(), color.blue());
            color_name_info.0
        } else {
            ""
        };

        let brand = if !slicer_name.is_empty() {
            get_brand_from_text(slicer_name.as_str()).unwrap_or("")
        } else {
            ""
        };
        let filament_title = format!("{brand} {material} {color_name}").trim().to_shared_string();
        let available_in_spool = self.weight_left(tray).unwrap_or_default();

        let pa = match tray.cali_idx {
            Some(-1) | Some(0) | None => {
                slint::format!("({})", tray.k_from_tray.unwrap_or(0.02))
            }
            Some(cali_idx) => {
                let (k_value, profile_name) = printer_borrow
                    .calibrations
                    .iter()
                    .find(|c| c.cali_idx == cali_idx)
                    .map_or(("(0.02)", ""), |c| (c.k_value.as_str(), c.name.as_str()));
                slint::format!("{k_value}, {profile_name}")
            }
        };

        UiSlotDisplay {
            available_in_spool,
            color_code,
            consumed_since_loaded: tray.meta_info.consumed_since_load,
            filament_title,
            slicer_name,
            temp_max,
            temp_min,
            pa,
        }
    }

    fn ui_get_spool_tag_definition_display(
        &self,
        tag_definition_type: &str,
        tag_definition_info: &str,
        empty_spool_weight: i32,
    ) -> UiSpoolRecordDisplay {
        let mut spool_rec = match tag_definition_type {
            BAMBULAB_TAG_TYPE => {
                if let Ok(bambu_tag) = serde_json::from_str::<BambuLabTag>(tag_definition_info) {
                    bambu_tag.to_spool_rec()
                } else {
                    return UiSpoolRecordDisplay::default();
                }
            }
            OPENPRINTTAG_TAG_TYPE => {
                if let Ok(open_print_tag) = serde_json::from_str::<OpenPrintTagTag>(tag_definition_info) {
                    match open_print_tag.to_spool_rec() {
                        Ok(spool_rec) => spool_rec,
                        Err(_err) => {
                            error!("Error parsing OpenPrintTag tag");
                            return UiSpoolRecordDisplay {
                                spool_record: UiSpoolRecord {
                                    note: "Error parsing OpenPrintTag tag".into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            };
                        }
                    }
                } else {
                    return UiSpoolRecordDisplay::default();
                }
            }
            _ => {
                error!("Internal Error, unexpected tag definition type");
                return UiSpoolRecordDisplay::default();
            }
        };

        if empty_spool_weight != 0 {
            spool_rec.weight_core = Some(empty_spool_weight);
        }

        let record = UiSpoolRecord {
            brand: spool_rec.brand.into(),
            color_code: spool_rec.color_code.into(),
            color_name: spool_rec.color_name.into(),
            material_type: spool_rec.material_type.into(),
            material_subtype: spool_rec.material_subtype.into(),
            slicer_filament: spool_rec.slicer_filament.into(),
            weight_advertised: spool_rec.weight_advertised.unwrap_or_default(),
            weight_core: spool_rec.weight_core.unwrap_or_default(),
            note: spool_rec.note.into(),
            ..Default::default()
        };

        let (slicer_filament_name, temp_min, temp_max) = if let Some(filament_info) = &self.get_filament_info(&record.slicer_filament, None) {
            (
                slint::format!(
                    "{}{}",
                    filament_info.slicer_name,
                    if filament_info.base_filament { " (base)" } else { "" }
                ),
                filament_info.nozzle_temp_low,
                filament_info.nozzle_temp_high,
            )
        } else {
            Default::default()
        };

        let color = if record.color_code.len() >= 6 {
            let color = u32::from_str_radix(&record.color_code[..6], 16).unwrap() + 0xFF000000; // the plus 0xFF at the end is fo add alpha
            slint::Color::from_argb_encoded(color)
        } else {
            slint::Color::default()
        };

        UiSpoolRecordDisplay {
            slicer_filament_name,
            spool_record: record,
            temp_min,
            temp_max,
            color,
            ..Default::default()
        }
    }

    fn ui_get_spool_record_display(&self, ui_spool_id: &SharedString) -> UiSpoolRecordDisplay {
        if ui_spool_id.is_empty() {
            return UiSpoolRecordDisplay::default();
        }
        let spool_rec = self.store.get_spool_by_id(ui_spool_id.as_str());
        if spool_rec.is_none() {
            return UiSpoolRecordDisplay::default();
        }
        let spool_rec = spool_rec.unwrap();

        let weight_left = self.weight_left_spool(&spool_rec, None);
        let weight_left = weight_left.map_or_else(String::new, |f| format!("{:.1}", f));
        let weight_left = weight_left.trim_end_matches('0').trim_end_matches('.');
        let weight_left = if weight_left.is_empty() {
            SharedString::new()
        } else {
            slint::format!("{}g", weight_left)
        };

        let record = UiSpoolRecord {
            added_full: spool_rec.added_full.unwrap_or_default(),
            // added_time: todo!(),
            brand: spool_rec.brand.into(),
            color_code: spool_rec.color_code.into(),
            color_name: spool_rec.color_name.into(),
            consumed_since_add: spool_rec.consumed_since_add,
            consumed_since_weight: spool_rec.consumed_since_weight,
            // encode_time: todo!(),
            ext_has_k: spool_rec.ext_has_k,
            id: spool_rec.id.into(),
            material_type: spool_rec.material_type.into(),
            material_subtype: spool_rec.material_subtype.into(),
            note: spool_rec.note.into(),
            slicer_filament: spool_rec.slicer_filament.into(),
            tag_id: spool_rec.tag_id.into(),
            weight_advertised: spool_rec.weight_advertised.unwrap_or_default(),
            weight_core: spool_rec.weight_core.unwrap_or_default(),
            weight_current: spool_rec.weight_current.unwrap_or_default(),
            weight_new: spool_rec.weight_new.unwrap_or_default(),
        };

        // for now, on purpose, not filling in fields that aren't in the tag, to show the real tag information
        let (slicer_filament_name, temp_min, temp_max) = if let Some(filament_info) = &self.get_filament_info(&record.slicer_filament, None) {
            (
                slint::format!(
                    "{}{}",
                    filament_info.slicer_name,
                    if filament_info.base_filament { " (base)" } else { "" }
                ),
                filament_info.nozzle_temp_low,
                filament_info.nozzle_temp_high,
            )
        } else {
            Default::default()
        };

        let color = if record.color_code.len() >= 6 {
            let color = u32::from_str_radix(&record.color_code[..6], 16).unwrap() + 0xFF000000; // the plus 0xFF at the end is fo add alpha
            slint::Color::from_argb_encoded(color)
        } else {
            slint::Color::default()
        };

        UiSpoolRecordDisplay {
            pa_line1: (if record.ext_has_k { "Configured" } else { "Not Configured" }).to_shared_string(),
            pa_line2: SharedString::new(),
            slicer_filament_name,
            spool_record: record,
            temp_min,
            temp_max,
            color,
            weight_left,
        }
    }

    fn tag_info_to_ui_spool_info_direct(
        &self,
        _bambu_printer_borrow: &BambuPrinter,
        full_spool_rec: &Option<FullSpoolRecord>,
    ) -> Option<crate::app::UiSpoolRecordDisplay> {
        full_spool_rec.as_ref().map(|full_spool_rec| self.ui_get_spool_record_display(&full_spool_rec.spool_rec.id.to_shared_string()))
    }

    fn update_ui_from_printer(&self, bambu_printer: &BambuPrinter) {
        // note - accepting bambu_printer rather than taking from self, because it may be called during callback on_trays_update,
        // and that's taking place when it's already borrowed and another borrow will panic

        let current_selected_printer = self.bambu_printer_model.index;
        if bambu_printer.printer_index != current_selected_printer {
            warn!("Internal Error: Requested to update UI for non active printer");
            return;
        }

        let ui = self.ui_weak.unwrap();
        ui.global::<crate::app::AppState>()
            .set_num_extruders(bambu_printer.num_extruders() as i32);
        // ----- handle number of ams's and curr_ams -----
        // OPT: calculate only when ams_exists change (store in printer struct), here use the value calculated there
        //      don't forget to consider loading the ams_exist from state which will need to recalculate, so add inner_set_ams_exist_bits
        if let Some(mut ams_exist_bits) = bambu_printer.ams_exist_bits() {
            let mut ams_exist_vec = Vec::<i32>::new();
            let mut first_ams = -1;
            for ams_id in 0..=3 + 8 {
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
        // OPT: run only on real trays (consider also AMS-HT), use the ams_exists from above
        for tray_row in 0..trays_state.row_count() {
            let ui_tray_id = trays_state.row_data(tray_row).unwrap().id;
            let curr_tray = match ui_tray_id {
                255 => &bambu_printer.virt_trays()[0],
                254 => &bambu_printer.virt_trays()[1],
                _ => &bambu_printer.ams_trays()[ui_tray_id as usize],
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
            if let Some(spool_id) = &curr_tray.meta_info.spool_id {
                ui_tray.tagged = true;
                ui_tray.spool_rec_id = spool_id.into();
            } else {
                ui_tray.tagged = false;
                ui_tray.spool_rec_id = SharedString::new();
            }
            // let k_value_unformatted = curr_tray.k.as_ref().unwrap_or(&"(0.020)".to_string()).clone();
            let k_value_unformatted = bambu_printer.get_tray_resolved_k_value(curr_tray, ui_tray_id);
            // let k_value_for_ui = k_value_for_ui(&k_value_unformatted);
            ui_tray.k = SharedString::from(k_value_unformatted);
            ui_tray.weight_display = self.weight_display(curr_tray);
            ui_tray.used_in_print = curr_tray.meta_info.used_in_print;
            trays_state.set_row_data(tray_row, ui_tray);
        }
    }

    fn weight_display(&self, tray: &Tray) -> SharedString {
        if let Some(weight_left) = self.weight_left(tray) {
            slint::format!("{:.1}g", weight_left)
        } else if tray.meta_info.consumed_since_load != 0.0 {
            slint::format!("-{:.1}g", tray.meta_info.consumed_since_load)
        } else {
            SharedString::new()
        }
    }

    fn weight_left(&self, tray: &Tray) -> Option<f32> {
        // OPT: don'e access spool_rec, cache required data in meta_info / cache all spool_rec and update on changes in store using events
        let spool_id = tray.meta_info.spool_id.as_ref()?;
        let spool = self.store.get_spool_by_id(spool_id)?;
        self.weight_left_spool(&spool, Some(tray.meta_info.consumed_since_weight))
    }

    fn weight_left_spool(&self, spool: &SpoolRecord, consumed_since_weight_override: Option<f32>) -> Option<f32> {
        let mut weight_left = None;
        let weight_current = spool.weight_current?;
        let consumed_since_weight = consumed_since_weight_override.unwrap_or(spool.consumed_since_weight);
        if let Some(weight_core) = spool.weight_core {
            let realtime_weight = (weight_current - weight_core) as f32 - consumed_since_weight;
            weight_left = Some(realtime_weight);
        } else if let (Some(weight_new), Some(weight_advertised)) = (spool.weight_new, spool.weight_advertised) {
            let realtime_weight = (weight_current - (weight_new - weight_advertised)) as f32 - consumed_since_weight;
            weight_left = Some(realtime_weight);
        }
        weight_left
    }

    fn try_dispatch_next_gcode_job(&mut self) {
        let console_tls_slots_capacity = 100;// with new esp-mbedtls seems no limit, // 3 - self.bambu_printer_model.printers.len(); // per memory available
        let scale_tls_slots_capacity: usize = 100; // if self.app_config.borrow().is_scale_available() { 4 } else { 0 };
        let console_tls_slots_used: usize = self
            .gcode_jobs
            .iter()
            .filter(|job| job.job_location == GcodeJobLocation::Console)
            .map(|job| job.tls_slots)
            .sum();
        let scale_tls_slots_used: usize = self
            .gcode_jobs
            .iter()
            .filter(|job| job.job_location == GcodeJobLocation::Scale)
            .map(|job| job.tls_slots)
            .sum();

        let console_running_jobs_count = self.gcode_jobs.iter().filter(|job| job.job_location == GcodeJobLocation::Console).count();

        // need to put this only here because of rust borrow checker
        let first_pending_gcode_job = self.gcode_jobs.iter_mut().find(|job| job.job_location == GcodeJobLocation::Pending);
        if first_pending_gcode_job.is_none() {
            return;
        }

        let gcode_job = first_pending_gcode_job.unwrap();

        if console_tls_slots_capacity - console_tls_slots_used >= gcode_job.tls_slots {
            info!("Running gcode analysis job {} in Console", gcode_job.job_number);
            // if we have enough slots for this task in the console, give priority to console
            if self.console_available_gcode_tasks <= console_running_jobs_count {
                // if no tasks ready, launch new task and pass data directly
                info!(
                    "Launching a new fetch_gcode_analysis_task task # {}",
                    self.console_available_gcode_tasks + 1
                );
                self.framework
                    .borrow()
                    .spawner
                    .spawn_heap({
                        let trait_for_gcode_analyzer_rc: Rc<RefCell<dyn GcodeAnalyzerObserver>> = self.view_model.clone().unwrap();
                        let trait_for_gcode_analyzer_weak: Weak<RefCell<dyn GcodeAnalyzerObserver>> = Rc::downgrade(&trait_for_gcode_analyzer_rc);
                        fetch_gcode_analysis_task(
                            self.framework.clone(),
                            self.gcode_analysis_request_channel.clone(),
                            self.gcode_analysis_notification_channel.clone(),
                            trait_for_gcode_analyzer_weak,
                            gcode_job.analysis_request.take(),
                        )
                    })
                    .ok();
                self.console_available_gcode_tasks += 1;
                gcode_job.job_location = GcodeJobLocation::Console;
            } else {
                // if there are already tasks waiting for requests use them
                debug!("Using an existing console fetch_gcode_analysis_task task");
                let gcode_analysis_request = gcode_job.analysis_request.take().unwrap();
                match self.gcode_analysis_request_channel.try_send(gcode_analysis_request) {
                    Ok(_) => gcode_job.job_location = GcodeJobLocation::Console,
                    Err(err) => {
                        error!("Failed sending request for gcode analysis within console : {err:?}");
                    }
                }
            }
        } else if scale_tls_slots_capacity - scale_tls_slots_used >= gcode_job.tls_slots {
            // dispatch to scale
            info!("Dispatching gcode analysis job {} to Scale", gcode_job.job_number);
            let gcode_analysis_request = gcode_job.analysis_request.take().unwrap();
            match self.spool_scale_model.borrow_mut().request_gcode_analysis(gcode_analysis_request) {
                Ok(_) => gcode_job.job_location = GcodeJobLocation::Console,
                Err(err) => {
                    error!("Failed sending request for gcode analysis to scale : {err:?}");
                }
            }
        } else {
            debug!(
                "No resources to run gcode analysis job {}, waiting for resources to free",
                gcode_job.job_number
            );
        };
    }

    pub fn get_k_info_from_old_tag(&self, tag_with_k: &TagInformationV1) -> Option<KInfo> {
        if !tag_with_k.calibrations.is_empty() {
            let calibration = tag_with_k.calibrations.iter().next().unwrap();
            let diameter = calibration.0;
            // because for security reasond in the tag the serial is hashed, can't reverse
            // so need to run over all printers and search for a matching printer
            let mut printer_found = None;
            if !tag_with_k.calibrations_printer_uuid.is_empty() {
                for printer in &self.bambu_printer_model.printers {
                    let printer_borrow = printer.borrow();
                    if printer_borrow.printer_uuid_to_encode == tag_with_k.calibrations_printer_uuid {
                        printer_found = Some(printer.clone());
                    }
                }
            }

            if printer_found.is_none() && !tag_with_k.calibrations_printer_name.is_empty() {
                for printer in &self.bambu_printer_model.printers {
                    let printer_borrow = printer.borrow();
                    if *printer_borrow.printer_name() == tag_with_k.calibrations_printer_name {
                        printer_found = Some(printer.clone());
                    }
                }
            }

            if let Some(printer) = printer_found {
                let printer_borrow = printer.borrow();
                return Some(KInfo {
                    printers: HashMap::from([(
                        printer_borrow.printer_serial.clone(),
                        KPrinter {
                            extruders: HashMap::from([(
                                0,
                                KExtruder {
                                    diameters: HashMap::from([(
                                        diameter.clone(),
                                        KNozzleDiameter {
                                            nozzles: HashMap::from([(
                                                "".to_string(),
                                                KNozzleId {
                                                    name: calibration.1.name.clone(),
                                                    k_value: calibration.1.k_value.clone(),
                                                    setting_id: calibration.1.setting_id.clone(),
                                                    cali_idx: calibration.1.cali_idx,
                                                },
                                            )]),
                                        },
                                    )]),
                                },
                            )]),
                        },
                    )]),
                });
            }
        }
        None
    }

    fn display_filament_staging_direct(&self, bambu_printer_borrow: &BambuPrinter, finish_operation: bool) {
        let filament_staging_borrow = self.filament_staging.borrow();
        if let Some(ui_spool_info) = self.tag_info_to_ui_spool_info_direct(bambu_printer_borrow, filament_staging_borrow.full_spool_rec()) {
            let ui = self.ui_weak.clone();
            if *filament_staging_borrow.origin() == StagingOrigin::Scanned {
                ui.unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_read_tag_succeeded(ui_spool_info, finish_operation);
            } else if *filament_staging_borrow.origin() == StagingOrigin::Encoded && finish_operation {
                ui.unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_update_spool_staging(ui_spool_info.clone(), crate::app::SpoolStagingState::Encoded);
            } else if *filament_staging_borrow.origin() == StagingOrigin::Unloaded && finish_operation {
                ui.unwrap().global::<crate::app::AppState>().invoke_tag_unloaded(ui_spool_info);
            } else {
                ui.unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_update_spool_staging(ui_spool_info.clone(), crate::app::SpoolStagingState::Unchanged);
            }
        }
    }

    fn display_filament_staging(&self, notify_operation: bool) {
        let bambu_printer_borrow = self.bambu_printer_model.borrow();
        self.display_filament_staging_direct(&bambu_printer_borrow, notify_operation);
    }

    fn dispatch_async_task(&self, async_task_request: AppAsyncTaskRequest) -> Result<(), String> {
        match self.app_async_tasks_channel.try_send(async_task_request.clone()) {
            Ok(_) => Ok(()),
            Err(err) => {
                error!("Error processing main app async task : {async_task_request:?} : {err:?}");
                Err(format!("Error dispathinc async task : {err:?}"))
            }
        }
    }

    pub fn update_spool_weight_from_button(&self, scale_weight: ScaleWeight) -> Option<bool> {
        if self.filament_staging.borrow().full_spool_rec().is_some() {
            match scale_weight {
                ScaleWeight::Stable(weight) => {
                    if weight == 0 {
                        info!("User Error: Reqeust to store tag with no weight on scale");
                        self.message_box(
                            "Scale Notice",
                            "No Weight on Scale",
                            "Can't Update Spool Weight",
                            crate::app::StatusType::Error,
                            -1,
                        );
                        Some(false)
                    } else if let Some(spool_id) = self.filament_staging.borrow().spool_rec().map(|sr| sr.id.clone()) {
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetSpoolWeight {
                            spool_id,
                            weight_current: weight,
                            weight_new: -1,
                            final_step: false,
                            from_button: true,
                        });
                        Some(true)
                    } else {
                        self.message_box(
                            "Staging Notice",
                            "No Spool in Staging\nCan't Update Spool Weight",
                            "See Documentation for More Details",
                            crate::app::StatusType::Error,
                            0,
                        );
                        Some(false)
                    }
                }
                ScaleWeight::Unstable(_) => {
                    info!("User Error: Reqeust to store tag with weight but scale weight is not stable");

                    self.message_box(
                        "Scale Notice",
                        "Weight on Scale Not Stable",
                        "Can't Update Spool Weight",
                        crate::app::StatusType::Error,
                        -1,
                    );
                    Some(false)
                    // TODO: notify on GUI and on Scale Led
                }
                ScaleWeight::Unknown => {
                    info!("Software Error: scale weight unknown after connect?");
                    self.message_box(
                        "Software Notice",
                        "Internal Software Error",
                        "Can't Update Spool Weight",
                        crate::app::StatusType::Error,
                        -1,
                    );
                    Some(false)
                }
            }
        } else {
            info!("User Error: Reqeust to store tag with weight but no tag information in staging");

            self.message_box(
                "Staging Notice",
                "No Spool in Staging\nCan't Update Spool Weight",
                "See Documentation for More Details",
                crate::app::StatusType::Error,
                -1,
            );
            Some(false)
            // TODO:  notify on GUI and on Scale Led
        }
    }

    // returns false if needs to be processed as a standard tag, true if v1 whether error or not
    pub fn process_v1_tag_read(&self, tag_uid: &[u8], tag: &str, _scanned_on_scale: bool) -> bool {
        let ui = self.ui_weak.clone();
        // TODO: When moving to no need to encode tag, displaying here in staging should only take place
        // if there is data from store. All processing here will be only to import old tags not in store
        if let Ok(tag_info) = TagInformationV1::from_v1_descriptor(tag) {
            // we need to store tag on read in two cases:
            // Tag with this tag_id is not in store  - for upgrading from non inventory release to inventory release
            // Tag with this tag_id is in store, but w/o K there, and the tag has K - for upgrading from old tags with K to new K approach
            // if let Some(mut tag_info) = tag_info_clone {
            if self.store.exists_tag_id(tag_uid) {
                return false;
            }
            if let Some(tag_id) = &tag_info.tag_id {
                if let Some(spool_rec) = self.store.get_spool_by_tag_id(tag_id) {
                    // tag-id is already in store
                    // Note: this is a special case where the tag_id on the spool is different than the tag_uid,
                    // should only happen in case of old bugs in uid or in case of copy of tags content
                    ui.unwrap().global::<crate::app::AppState>().invoke_show_message_box(
                        slint::format!("Tag {} Read Notice", spool_rec.id),
                        "A Previous Version Tag Conflicting Tag-Id's".into(),
                        "It May Require Re-Configuring".into(),
                        crate::app::StatusType::Normal,
                        0,
                    );
                    self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Scanned);
                    self.display_filament_staging(true);
                } else {
                    ui.unwrap()
                        .global::<crate::app::AppState>()
                        .invoke_new_v1_tag_scanned(hex::encode_upper(tag_uid).into(), tag.into());
                }
            } else {
                error!("Error with scanned V1 tag - old tag read with no tag_id");
                ui.unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_read_tag_failed(SharedString::from("V1 Tag Missing Tag-ID"));
            }
            true
        } else {
            false
        }
    }

    pub async fn process_v1_tag_read_async(view_model: Rc<RefCell<ViewModel>>, tag_id_hex: String, tag: String, final_step: bool) {
        debug!("Received to process async read tag {tag}");

        // TODO: Can there be here a flow that doesn't need to store a new tag?????
        // I think not because if tag is available then not storing anything and not reaching here at all (to the async fn)

        // Using the real tag uid to store and not the content of the tag in this case
        if let Ok(tag_info) = TagInformationV1::from_v1_descriptor(&tag) {
            // if let Some(_tag_info_tag_id) = &tag_info.tag_id {
            // we need to store tag on read in two cases:
            // Tag with this tag_id is not in store  - for upgrading from non inventory release to inventory release
            // Tag with this tag_id is in store, but w/o K there, and the tag has K - for upgrading from old tags with K to new K approach
            // if let Some(mut tag_info) = tag_info_clone {
            let tag_id = match hex::decode(&tag_id_hex) {
                Ok(tag_id) => tag_id,
                Err(err) => {
                    let ui = view_model.borrow().ui_weak.unwrap();
                    let ui_app_state = ui.global::<crate::app::AppState>();
                    error!("Error in v1 tag import flow converting tag uid (not in tag content) from hex : {err}");
                    ui_app_state.invoke_show_message_box(
                        "Tag Import Notice".into(),
                        "Unexpected Internal Error".into(),
                        "Bad Tag-Id to Store".into(),
                        crate::app::StatusType::Error,
                        0,
                    );
                    return;
                }
            };
            let (spool_rec, need_to_store, tag_k_info) = {
                let spool_rec = view_model.borrow().store.get_spool_by_tag_id(&tag_id);
                let mut need_to_store = spool_rec.is_none();

                let mut k_info = None;
                if !tag_info.calibrations.is_empty() {
                    let need_to_store_k = if let Some(spool_rec) = &spool_rec { !spool_rec.ext_has_k } else { true };
                    need_to_store |= need_to_store_k;
                    if need_to_store_k {
                        k_info = view_model.borrow().get_k_info_from_old_tag(&tag_info);
                    }
                }
                (spool_rec, need_to_store, k_info)
            };

            let store = view_model.borrow().store.clone();
            if need_to_store {
                let (res, spool_rec_id, spool_rec_ext) = if let Some(mut spool_rec) = spool_rec {
                    // spool_rec already availble, need to only deal with storing K if exists
                    // we know there's k_info here, because otherwise need_to_store wouldn't be true (wouldn't be a reason to store anything)
                    spool_rec.ext_has_k = true;
                    let spool_rec_id = spool_rec.id.clone();
                    match store.update_spool(spool_rec, Some(Box::new(move |ext| ext.k_info = tag_k_info))).await {
                        Ok(spool_rec_ext) => (Ok(()), spool_rec_id, spool_rec_ext),
                        Err(e) => (Err(e), spool_rec_id, None),
                    }
                } else {
                    // spool_rec not available, meaning a new record to add
                    let mut new_spool_rec = tag_info.to_spool_rec();
                    new_spool_rec.tag_id = hex::encode_upper(tag_id); // replace the tag_id with the real tag uid
                    new_spool_rec.ext_has_k = tag_k_info.is_some();
                    let new_spool_rec_ext = SpoolRecordExt {
                        tag: Some(tag.clone()),
                        k_info: tag_k_info,
                        origin_data: Some(OriginData::SpoolEaseV1 { uid: tag_id_hex, url: tag }),
                    };
                    match store.add_spool(new_spool_rec.clone(), new_spool_rec_ext.clone()).await {
                        Ok(new_spool_rec_id) => (Ok(()), new_spool_rec_id, Some(new_spool_rec_ext)),
                        Err(e) => (Err(e), String::new(), Some(new_spool_rec_ext)),
                    }
                };

                let ui = view_model.borrow().ui_weak.unwrap();
                let ui_app_state = ui.global::<crate::app::AppState>();
                let view_model_borrow = view_model.borrow();
                match res {
                    Ok(_) => {
                        if let Some(spool_rec) = view_model_borrow.store.get_spool_by_id(&spool_rec_id) {
                            view_model_borrow
                                .filament_staging
                                .borrow_mut()
                                .set_spool_record(spool_rec, StagingOrigin::Scanned);
                            if let Some(spool_rec_ext) = spool_rec_ext {
                                view_model_borrow.filament_staging.borrow_mut().set_spool_record_ext(spool_rec_ext);
                            }
                            view_model.borrow().display_filament_staging(final_step);
                            ui_app_state.invoke_add_v1_tag_to_inventory_status(SharedString::new(), spool_rec_id.to_shared_string());
                        } else {
                            ui_app_state.invoke_show_message_box(
                                "Critical Store Notice".into(),
                                "Unexpected Error".into(),
                                "Failed to Get Spool After Storing It".into(),
                                crate::app::StatusType::Error,
                                -1,
                            );
                        }
                    }
                    Err(err) => {
                        ui_app_state.invoke_show_message_box(
                            "Critical Store Notice".into(),
                            "Failed to store information from tag".into(),
                            err.to_shared_string(),
                            crate::app::StatusType::Error,
                            -1,
                        );
                    }
                }
            } else if let Ok(spool_rec_ext) = store.get_spool_ext_by_id(&spool_rec.unwrap().id).await {
                view_model.borrow().filament_staging.borrow_mut().set_spool_record_ext(spool_rec_ext);
                view_model.borrow().display_filament_staging(true);
            }
            // } else {
            //     let ui = view_model.borrow().ui_weak.unwrap();
            //     let ui_app_state = ui.global::<crate::app::AppState>();
            //     error!("Tag is missing tag id : {tag}");
            //     ui_app_state.invoke_show_message_box(
            //         "Old Tag Notice".into(),
            //         "Tag is Missing Tag-Id".into(),
            //         SharedString::new(),
            //         crate::app::StatusType::Error,
            //         -1,
            //     );
            // }
        } else {
            let ui = view_model.borrow().ui_weak.unwrap();
            let ui_app_state = ui.global::<crate::app::AppState>();
            error!("Cant parse tag descriptor {tag}");
            ui_app_state.invoke_show_message_box(
                "Old Tag Notice".into(),
                "Cant Parse Tag Descriptor".into(),
                SharedString::new(),
                crate::app::StatusType::Error,
                -1,
            );
        }
    }

    async fn import_definition_tag_to_inventory_async(
        view_model: Rc<RefCell<ViewModel>>,
        tag_definition_type: String,
        tag_definition_info: String,
        empty_spool_weight: i32,
    ) {
        let (spool_rec, origin_data) = match tag_definition_type.as_str() {
            BAMBULAB_TAG_TYPE => {
                if let Ok(bambulab_tag) = serde_json::from_str::<BambuLabTag>(&tag_definition_info) {
                    (Some(bambulab_tag.to_spool_rec()), Some(OriginData::BambuLabTag { bambulab_tag }))
                } else {
                    (None, None)
                }
            }
            OPENPRINTTAG_TAG_TYPE => {
                if let Ok(openprinttag_tag) = serde_json::from_str::<OpenPrintTagTag>(&tag_definition_info) {
                    match openprinttag_tag.to_spool_rec() {
                        Ok(spool_rec) => (Some(spool_rec), Some(OriginData::OpenPrintTagTag { openprinttag_tag })),
                        Err(_err) => {
                            error!("Error parsing OpenPrintTag tag");
                            (None, None)
                        }
                    }
                } else {
                    (None, None)
                }
            }
            _ => {
                error!("Internal Error, unexpected tag definition type");
                (None, None)
            }
        };
        if let Some(mut new_spool_rec) = spool_rec {
            if empty_spool_weight != 0 {
                new_spool_rec.weight_core = Some(empty_spool_weight);
            }
            let new_spool_rec_ext = SpoolRecordExt {
                tag: None,
                k_info: None,
                origin_data,
            };

            let store = view_model.borrow().store.clone();
            let ui = view_model.borrow().ui_weak.unwrap();
            let ui_app_state = ui.global::<crate::app::AppState>();
            match store.add_spool(new_spool_rec, new_spool_rec_ext).await {
                Ok(new_spool_rec_id) => {
                    info!("Added new Bambulab Spool record number {new_spool_rec_id}");
                    view_model.borrow_mut().recently_added_spool_id = Some(new_spool_rec_id.clone());
                    ui_app_state.invoke_import_definition_tag_to_inventory_status("".into(), new_spool_rec_id.into());
                }
                Err(err) => {
                    error!("Failed to add bambulab spool record {err:?}");
                    ui_app_state.invoke_show_message_box(
                        "Critical Store Notice".into(),
                        "Failed to store information from tag".into(),
                        err.to_shared_string(),
                        crate::app::StatusType::Error,
                        -1,
                    );
                }
            };
        }
    }

    async fn link_tag_to_spool_id_async(view_model: Rc<RefCell<ViewModel>>, tag_id: String, tag_type: String, spool_id: String, final_step: bool) {
        let store = view_model.borrow().store.clone();
        if let Some(mut spool_rec) = store.get_spool_by_id(&spool_id) {
            spool_rec.tag_id = tag_id.clone();
            spool_rec.tag_type = tag_type.clone();
            let store_res = store.update_spool(spool_rec.clone(), None).await;
            let ui = view_model.borrow().ui_weak.unwrap();
            let ui_app_state = ui.global::<crate::app::AppState>();
            match store_res {
                Ok(_) => {
                    ui_app_state.invoke_link_tag_to_spool_id_status(SharedString::new());
                    view_model
                        .borrow()
                        .filament_staging
                        .borrow_mut()
                        .set_spool_record(spool_rec, StagingOrigin::Scanned);
                    view_model.borrow().display_filament_staging(final_step);
                    Self::set_staging_rec_ext_async(view_model.clone()).await;
                }
                Err(err) => {
                    error!("Failed to link tag {tag_id} to spool_id {spool_id}: {err:?}");
                    ui_app_state.invoke_link_tag_to_spool_id_status(format!("Failed to link tag to spool {spool_id}: {err:?}").to_shared_string());
                }
            }
        } else {
            error!("Failed to link tag {tag_id} to spool_id {spool_id}: Spool Id not Found");
            let ui = view_model.borrow().ui_weak.unwrap();
            let ui_app_state = ui.global::<crate::app::AppState>();
            ui_app_state.invoke_link_tag_to_spool_id_status(format!("Spool Id {spool_id} Not Found").to_shared_string());
        }
    }

    // this unlinks a single tag from a spool-id, currently there's only one, bug in the future may be several
    async fn unlink_spool_from_tag_async(view_model: Rc<RefCell<ViewModel>>, spool_id: String) {
        let store = view_model.borrow().store.clone();
        if let Some(mut spool_rec) = store.get_spool_by_id(&spool_id) {
            let unlinked_tag_id = core::mem::take(&mut spool_rec.tag_id);
            spool_rec.tag_type = "".to_string();
            spool_rec.encode_time = None;
            let store_res = store.update_spool(spool_rec.clone(), None).await;
            let ui = view_model.borrow().ui_weak.unwrap();
            let ui_app_state = ui.global::<crate::app::AppState>();
            match store_res {
                Ok(_) => {
                    view_model.borrow().filament_staging.borrow_mut().clear();
                    ui_app_state.invoke_empty_spool_staging();
                    ui_app_state.invoke_unlink_spool_id_from_tag_status(spool_id.to_shared_string(), SharedString::new());
                }
                Err(err) => {
                    error!("Failed to unlink spool_id {spool_id} from tag {unlinked_tag_id}: {err:?}");
                    ui_app_state.invoke_unlink_spool_id_from_tag_status(spool_id.to_shared_string(), err.to_string().into());
                }
            }
        } else {
            let ui = view_model.borrow().ui_weak.unwrap();
            let ui_app_state = ui.global::<crate::app::AppState>();
            error!("Failed to unlink spool_id {spool_id} from tag: Spool Id not found");
            ui_app_state.invoke_unlink_spool_id_from_tag_status(spool_id.to_shared_string(), "Spool Id {spool_id} Not Found".to_string().into());
        }
    }
    async fn set_staging_rec_ext_async(view_model: Rc<RefCell<ViewModel>>) {
        let spool_id = {
            let view_model_borrow = view_model.borrow();
            let filament_staging = view_model_borrow.filament_staging.borrow();
            filament_staging.spool_rec().map(|spool_rec| spool_rec.id.clone())
        };
        if let Some(spool_id) = spool_id {
            let store = view_model.borrow().store.clone();
            if let Ok(spool_rec_ext) = store.get_spool_ext_by_id(&spool_id).await {
                view_model.borrow().filament_staging.borrow_mut().set_spool_record_ext(spool_rec_ext);
                view_model.borrow().display_filament_staging(false);
            }
        }
    }

    // weight_new: if -1 don't touch, otherwiser set value (and added_full)
    async fn set_spool_weight_async(
        view_model: Rc<RefCell<ViewModel>>,
        spool_id: String,
        weight_current: i32,
        weight_new: i32,
        final_step: bool,
        from_button: bool,
    ) {
        let store = view_model.borrow().store.clone();
        if let Some(mut spool_rec) = store.get_spool_by_id(&spool_id) {
            spool_rec.weight_current = Some(weight_current);
            spool_rec.consumed_since_weight = 0.0;
            if weight_new != -1 {
                spool_rec.weight_new = Some(weight_new);
                if weight_new != 0 {
                    spool_rec.added_full = Some(true);
                } // else - don't touch added-full
            } // else - don't touch weight-new
            match store.update_spool(spool_rec.clone(), None).await {
                Ok(_) => {
                    view_model.borrow().filament_staging.borrow_mut().update_spool_rec_keep_rest(spool_rec);
                    view_model.borrow().display_filament_staging(final_step);
                    let ui = view_model.borrow().ui_weak.unwrap();
                    let ui_app_state = ui.global::<crate::app::AppState>();
                    ui_app_state.invoke_updated_spool_weight(spool_id.into(), from_button);
                }
                Err(err) => {
                    error!("Failed to Update Spool {spool_id} Weight");
                    view_model.borrow().message_box(
                        "Inventory Notice",
                        &format!("Failed to Update Spool {spool_id} Weight"),
                        &err.to_string(),
                        crate::app::StatusType::Error,
                        0,
                    );
                }
            }
        } else {
            error!("Failed to Update Spool {spool_id} Weight: Spool Id not found");
            view_model.borrow().message_box(
                "Inventory Notice",
                &format!("Failed to Update Spool {spool_id} Weight"),
                "Spool Id Not Found",
                crate::app::StatusType::Error,
                0,
            );
        }
    }

    async fn update_spool_rec_async(view_model: Rc<RefCell<ViewModel>>, spool_rec: SpoolRecord) {
        let store = view_model.borrow().store.clone();
        match store.update_spool(spool_rec.clone(), None).await {
            Ok(_) => {
                let view_model_borrow = view_model.borrow();
                let need_replace_staging = if let Some(staging_spool_rec) = view_model_borrow.filament_staging.borrow().spool_rec() {
                    staging_spool_rec.id == spool_rec.id
                } else {
                    false
                };
                if need_replace_staging {
                    {
                        view_model_borrow.filament_staging.borrow_mut().update_spool_rec_keep_rest(spool_rec);
                    }
                    view_model_borrow.display_filament_staging(false);
                }
            }
            Err(_) => {
                let view_model_borrow = view_model.borrow();
                let ui = view_model_borrow.ui_weak.unwrap();
                let ui_app_state = ui.global::<crate::app::AppState>();
                info!("Error updating spool in store");

                ui_app_state.invoke_show_message_box(
                    "Critical Store Notice".into(),
                    "Error Updating Spool in Store".into(),
                    SharedString::new(),
                    crate::app::StatusType::Error,
                    -1,
                );
            }
        }
    }

    async fn configure_tray_with_spool_async(view_model: Rc<RefCell<ViewModel>>, tray_id: i32, spool_id: String) {
        let store = view_model.borrow().store.clone();
        if let Some(spool_rec) = store.get_spool_by_id(&spool_id) {
            let mut full_spool_rec = FullSpoolRecord {
                spool_rec,
                spool_rec_ext: SpoolRecordExt::default(),
            };
            if full_spool_rec.spool_rec.ext_has_k {
                match store.get_spool_ext_by_id(&spool_id).await {
                    Ok(spool_rec_ext) => {
                        full_spool_rec.spool_rec_ext = spool_rec_ext;

                        let view_model_borrow = view_model.borrow();
                        let tray_id_for_ui = tray_id;
                        let full_slot_description = Self::full_slot_description(&view_model.borrow().bambu_printer_model.borrow(), tray_id);
                        view_model_borrow
                            .ui_weak
                            .unwrap()
                            .global::<crate::app::AppState>()
                            .invoke_tray_update_succeeded(
                                full_slot_description.into(),
                                view_model_borrow.bambu_printer_model.borrow().printer_selector_name.to_shared_string(),
                                tray_id_for_ui,
                            );
                    }
                    Err(err) => {
                        error!("Failed to load Spool {spool_id} Extended Info when configuring slot");
                        view_model.borrow().message_box(
                            "Configure Slot Notice",
                            &format!("Error Loading Spool {spool_id} Extended Info"),
                            &err.to_string(),
                            crate::app::StatusType::Error,
                            0,
                        );
                    }
                }
            }

            let filament_info = view_model
                .borrow()
                .get_filament_info(&full_spool_rec.spool_rec.slicer_filament, Some(&full_spool_rec.spool_rec.material_type));
            if let Some(filament_info) = filament_info {
                view_model.borrow().bambu_printer_model.borrow_mut().set_tray_filament(
                    tray_id,
                    &full_spool_rec,
                    filament_info.nozzle_temp_low as u32,
                    filament_info.nozzle_temp_high as u32,
                );
            } else {
                error!("Failed to resolve filament temps for spool {spool_id}");
                view_model.borrow().message_box(
                    "Configure Slot Notice",
                    &format!("Spool {spool_id} Missing Required Information"),
                    "",
                    crate::app::StatusType::Error,
                    0,
                );
            }
        } else {
            error!("Spool {spool_id} not found when trying to configure slot {tray_id}");
            view_model.borrow().message_box(
                "Configure Slot Notice",
                &format!("Spool {spool_id} Not Found"),
                "",
                crate::app::StatusType::Error,
                0,
            );
        }
    }

    pub fn taks_screenshot(&self) -> Result<slint::SharedPixelBuffer<slint::Rgba8Pixel>, slint::PlatformError> {
        self.ui_weak.unwrap().window().take_snapshot()
    }

    pub fn get_spools_in_printers(&self) -> HashMap<String, String> {
        // spool_id,
        // location: A1 / B3 /... or Ext
        let mut locations = HashMap::new();
        let num_of_printers = self.bambu_printer_model.printers.len();
        for printer in &self.bambu_printer_model.printers {
            let printer_borrow = printer.borrow();
            for tray_id in (0..printer_borrow.ams_trays().len() - 1).chain([254, 255]) {
                // TODO: need to fix for H2 Series, deal with two external spools
                if let Some(spool_id) = &printer_borrow.get_any_tray(tray_id).meta_info.spool_id {
                    let (ams_id, slot_id) = BambuPrinter::get_ams_and_slot_id(tray_id);
                    let tray_name = match ams_id {
                        0..=3 => {
                            format!("{}{}", (b'A' + ams_id as u8) as char, slot_id + 1)
                        }
                        128..135 => {
                            format!("HT-{}", (b'A' + (ams_id as u8 - 128)) as char)
                        }
                        255 => {
                            if printer_borrow.num_extruders() == 1 {
                                "Ext".to_string()
                            } else {
                                "Ext Right".to_string()
                            }
                        }
                        254 => "Ext Left".to_string(),
                        _ => String::new(),
                    };
                    if num_of_printers > 1 {
                        locations.insert(spool_id.clone(), format!("{} @ {}", tray_name, printer_borrow.printer_name()));
                    } else {
                        locations.insert(spool_id.clone(), format!("{} @ Printer", tray_name));
                    }
                }
            }
        }
        locations
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_calibration_to_printer(
        &self,
        printer_serial: &str,
        extruder_id: i32,
        nozzle_diameter: &str,
        nozzle_id: &str,
        filament_id: &str,
        setting_id: &str,
        k_value: &str,
        name: &str,
    ) -> Result<(), String> {
        let mut found_printer = false;
        for printer in &self.bambu_printer_model.printers {
            let mut printer_borrow = printer.borrow_mut();
            if printer_borrow.printer_serial == printer_serial {
                printer_borrow.add_calibration_to_printer(extruder_id, nozzle_diameter, nozzle_id, filament_id, setting_id, k_value, name);
                found_printer = true;
                break;
            }
        }
        if found_printer {
            Ok(())
        } else {
            Err("Printer not found".to_string())
        }
    }

    pub fn update_firmware_versions(&self, fw: &[crate::app_ota::FirmwareInfo]) {
        let ui = self.ui_weak.unwrap();
        let ui_app_state: crate::app::AppState<'_> = ui.global::<crate::app::AppState>();
        let console_stable_info = fw
            .iter()
            .find(|fw| fw.product == AppOtaProduct::Console && fw.train == AppOtaTrain::Stable)
            .unwrap();
        let console_unstable_info = fw
            .iter()
            .find(|fw| fw.product == AppOtaProduct::Console && fw.train == AppOtaTrain::Unstable)
            .unwrap();
        let console_debug_info = fw
            .iter()
            .find(|fw| fw.product == AppOtaProduct::Console && fw.train == AppOtaTrain::Debug)
            .unwrap();
        let scale_stable_info = fw
            .iter()
            .find(|fw| fw.product == AppOtaProduct::Scale && fw.train == AppOtaTrain::Stable)
            .unwrap();
        let scale_unstable_info = fw
            .iter()
            .find(|fw| fw.product == AppOtaProduct::Scale && fw.train == AppOtaTrain::Unstable)
            .unwrap();
        let scale_debug_info = fw
            .iter()
            .find(|fw| fw.product == AppOtaProduct::Scale && fw.train == AppOtaTrain::Debug)
            .unwrap();
        let firmwares = crate::app::Firmwares {
            console_curr: self.framework.borrow_mut().settings.app_cargo_pkg_version.to_shared_string(),
            console_stable: console_stable_info.version.to_shared_string(),
            console_stable_newer: console_stable_info.newer,
            console_unstable: console_unstable_info.version.to_shared_string(),
            console_unstable_newer: console_unstable_info.newer,
            console_debug: console_debug_info.version.to_shared_string(),
            console_debug_newer: console_debug_info.newer,
            scale_curr: self.scale_version.clone().unwrap_or_default().to_shared_string(),
            scale_stable: scale_stable_info.version.to_shared_string(),
            scale_stable_newer: scale_stable_info.newer,
            scale_unstable: scale_unstable_info.version.to_shared_string(),
            scale_unstable_newer: scale_unstable_info.newer,
            scale_debug: scale_debug_info.version.to_shared_string(),
            scale_debug_newer: scale_debug_info.newer,
        };
        ui_app_state.invoke_notify_available_firmwares(firmwares);
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
    fn on_trays_update(
        &mut self,
        bambu_printer: &mut BambuPrinter,
        prev_trays_bits: &TrayBits,
        new_trays_bits: &TrayBits,
        removed_tags: &HashMap<usize, SpoolId>,
    ) {
        // note - accepting bambu_printer rather than taking from self, because it's already borrowed and another borrow will panic
        let current_selected_printer = self.bambu_printer_model.index;

        if bambu_printer.printer_index == current_selected_printer {
            self.update_ui_from_printer(bambu_printer);
        }

        // If staging is loaded from scanned/encoded then check spool load cases
        if ![StagingOrigin::Unloaded, StagingOrigin::Empty].contains(self.filament_staging.borrow().origin()) {
            // first - if there is a spool (from scan/encode) in staging, and a spool is loaded then
            // at the moment of loading notify ui so it can reset the staging timer in case it is too low
            // and won't reach read_done before timer is out
            if let Some(new_trays_monitored_bits) = new_trays_bits.tray_exist_bits {
                let prev_trays_monitored_bits = prev_trays_bits.tray_exist_bits.unwrap_or(0);
                let mut trays_monitored_loaded = Vec::new();
                for tray_id in 0..bambu_printer.ams_trays().len() {
                    let prev_tray_monitored_bit = ((prev_trays_monitored_bits >> tray_id) & 0x01) != 0;
                    let new_tray_monitored_bit = ((new_trays_monitored_bits >> tray_id) & 0x01) != 0;
                    if !prev_tray_monitored_bit && new_tray_monitored_bit {
                        trays_monitored_loaded.push(tray_id);
                    }
                }
                // if bambu_printer.printer_number == 1 { // UNREMARK FOR TESTS WITH ONE PRINTER
                if trays_monitored_loaded.len() == 1 {
                    self.ui_weak
                        .unwrap()
                        .global::<crate::app::AppState>()
                        .invoke_spool_loaded_when_staging_loaded();
                }
            }

            // ----- Handle loading when there is something in staging -----
            // If the staging is loaded and only a SINGLE slot SWITCHED to reading update it to the stating filament info
            // trace!("------------------------------------------------------");
            // trace!(">>>>> prev : {prev_trays_bits:?}\n >>>>> next: {new_trays_bits:?}");
            // trace!("------------------------------------------------------");
            if let Some(new_trays_monitored_bits) = new_trays_bits.tray_read_done_bits {
                let prev_trays_monitored_bits = prev_trays_bits.tray_read_done_bits.unwrap_or(0);
                let mut trays_monitored_loaded = Vec::new();
                for tray_id in 0..bambu_printer.ams_trays().len() {
                    let prev_tray_monitored_bit = ((prev_trays_monitored_bits >> tray_id) & 0x01) != 0;
                    let new_tray_monitored_bit = ((new_trays_monitored_bits >> tray_id) & 0x01) != 0;
                    if !prev_tray_monitored_bit && new_tray_monitored_bit {
                        trays_monitored_loaded.push(tray_id);
                    }
                }
                // if bambu_printer.printer_number == 1 { // UNREMARK FOR TESTS WITH ONE PRINTER
                if trays_monitored_loaded.len() == 1 {
                    let only_monitored_tray = trays_monitored_loaded[0];
                    info!("Single tray {only_monitored_tray} is loading now");
                    self.set_staging_to_tray_direct(
                        &self.filament_staging.clone(),
                        bambu_printer,
                        &self.ui_weak.clone(),
                        only_monitored_tray as i32,
                    );
                }
                // }
            }
        }

        // Unloaded spool case - load tag if exist on that spool to staging (for weighting)
        // take one of the unloaded tags (realistically there should be only one)
        if let Some(removed_spool) = removed_tags.iter().next() {
            if [StagingOrigin::Empty, StagingOrigin::Unloaded].contains(self.filament_staging.borrow().origin()) {
                // only if empty or was unloaded (so not scanned or encoded)
                if let Some(spool_rec) = self.store.get_spool_by_id(removed_spool.1) {
                    self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Unloaded);
                    self.display_filament_staging_direct(bambu_printer, true);
                    let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
                    // let _ = self.store.try_send_op(StoreOp::ReadExtInfo { id: removed_spool.1.clone() });
                }
            }
        }
    }

    fn on_printer_connect_status(&self, bambu_printer: &mut BambuPrinter, status: bool) {
        if status {
            // TODO: I can't borrow at this stage because my_mqtt reports this and need to borrow_mut so now can't borrow.
            //       Need to switch to the notifications coming from a notifier object and not directly from the objects.
            //       Or switch to a message loop notifications (which is a major change to the code, but more correct for these types of apps)
            //       So here I know it arrives here only if boot is successful, but in other applications this might not be enough
            // if self.app_config.borrow().boot_completed() {
            term_info!(&"-".repeat(62));
            term_info!("Printer [{}] connected successfully", bambu_printer.printer_number);
            term_info!(&"-".repeat(62));
            self.ui_weak
                .unwrap()
                .global::<crate::app::AppState>()
                .invoke_printer_connected(bambu_printer.printer_selector_name.to_shared_string());
        } else {
            term_info!("[{}] Printer disconnected", bambu_printer.printer_number);
        }
    }

    fn on_request_gcode_analysis(&mut self, printer: &mut BambuPrinter, print_project: &PrintProject) -> i32 {
        let ip = printer.printer_ip;
        let serial = printer.printer_serial.clone();
        let access_code = printer.printer_access_code.clone();
        let printer_number = printer.printer_number;
        let printer_index = printer.printer_index;
        let printer_selector_name = printer.printer_selector_name.clone();
        self.gcode_last_job_number += 1;

        let subtask_name = print_project.subtask_name.clone(); // subtask_name field in the project_file message
        let threemf_url = print_project.threemf_url.clone(); // url field
        let gcode_filename_in_3mf = print_project.gcode_filename_in_3mf.clone(); // the param field - file inside the gcode

        info!("[{printer_number}] Received request for gcode analysis {subtask_name} {gcode_filename_in_3mf}");

        let required_tls_slots = if printer.fetch_3mf == Fetch3mf::PrinterFtp
            || threemf_url.starts_with("file://")
            || threemf_url.starts_with("ftp://")
            || threemf_url.starts_with("brtc://")
        {
            // only in case of ftp, the number of FTP (not HTTP) tls slots depends on the printer model
            match printer.model_series() {
                bambu::PrinterModelSeries::Unknown => 2,
                bambu::PrinterModelSeries::X1 => 2,
                bambu::PrinterModelSeries::P1 => 1,
                bambu::PrinterModelSeries::A1 => 1,
                bambu::PrinterModelSeries::H2 => 2,
                bambu::PrinterModelSeries::P2 => 2,
            }
        } else {
            1
        };

        let chars_to_replace_for_file = match printer.model_series() {
            bambu::PrinterModelSeries::P1 | bambu::PrinterModelSeries::A1 => "!@#\'@/",
            bambu::PrinterModelSeries::X1 | bambu::PrinterModelSeries::H2 | bambu::PrinterModelSeries::P2 | bambu::PrinterModelSeries::Unknown => "/",
        };

        let threemf_ftp_filename: String = subtask_name
            .chars()
            .map(|c| if chars_to_replace_for_file.contains(c) { '_' } else { c })
            .collect();

        // if matches!(printer.model_series(),bambu::PrinterModelSeries::P2) {
        //     threemf_ftp_filename = format!("{}.gcode", threemf_ftp_filename);
        // }

        let ftp_memory_save = required_tls_slots == 1;

        let gcode_analysis_request = GcodeAnalysisRequest {
            fetch_3mf: printer.fetch_3mf,
            ip,
            serial,
            access_code,
            printer_number,
            printer_index,
            threemf_ftp_filename, // file in format /cache/... with 3mf added, why do it here?
            job_number: self.gcode_last_job_number,
            threemf_url,
            gcode_filename_in_3mf,
            ftp_memory_save,
            printer_selector_name,
        };

        self.gcode_jobs.push(GcodeJob {
            job_number: self.gcode_last_job_number,
            job_location: GcodeJobLocation::Pending,
            tls_slots: required_tls_slots,
            analysis_request: Some(gcode_analysis_request),
        });

        self.try_dispatch_next_gcode_job();

        self.gcode_last_job_number
    }

    fn on_cancel_gcode_analysis(&mut self, job_number: i32) {
        // first check if it happens to be a pending job, not submitted yet to processing
        let len_before = self.gcode_jobs.len();
        self.gcode_jobs
            .retain(|job| !(job.job_number == job_number && job.job_location == GcodeJobLocation::Pending));
        if self.gcode_jobs.len() < len_before {
            return;
        }
        // it wasn't pending, so lets send a request to cancel it
        self.gcode_analysis_notification_channel
            .immediate_publisher()
            .publish_immediate(GcodeAnalysisNotification::Cancel { job_number });
        if let Err(err) = self
            .spool_scale_model
            .borrow()
            .gcode_analysis_notify(GcodeAnalysisNotification::Cancel { job_number })
        {
            error!("Failed to send gcode analysis cancelation : {err}")
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
            Status::FoundTagNowErasing => {
                ui.unwrap().global::<crate::app::AppState>().invoke_erase_tag_found();
            }
            Status::WriteSuccess(_encoded_descriptor, cookie) => {
                // This call is triggered by a call from either spool_tag or spool_scale, so they are already borrowed.
                // They internally handle the switch from write to read for themselves, but not for the other.
                // So here we use the try_borrow to check who needs extra notification to stop writing
                if let Ok(encode_cookie) = serde_json::from_str::<SpoolEncodeCookie>(cookie) {
                    if let Some(mut spool_rec) = self.store.get_spool_by_id(&encode_cookie.spool_rec_id) {
                        spool_rec.encode_time = encode_cookie.encode_time;
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::UpdateSpoolRec { spool_rec });
                    }
                } else if let Ok(encode_cookie) = serde_json::from_str::<LocationEncodeCookie>(cookie) { 
                    debug!(">>>> TODO: Store the tag into DB, descriptor: {_encoded_descriptor}");
                }
                if let Ok(spool_tag_borrow) = self.spool_tag_model.try_borrow() {
                    spool_tag_borrow.read_tag();
                }
                if let Ok(spool_scale_borrow) = self.spool_scale_model.try_borrow() {
                    let _ = spool_scale_borrow.read_tag();
                }
                ui.unwrap().global::<crate::app::AppState>().invoke_encoding_succeeded();
            }
            Status::EraseSuccess => {
                if let Ok(spool_tag_borrow) = self.spool_tag_model.try_borrow() {
                    spool_tag_borrow.read_tag();
                }
                if let Ok(spool_scale_borrow) = self.spool_scale_model.try_borrow() {
                    let _ = spool_scale_borrow.read_tag();
                }
                ui.unwrap().global::<crate::app::AppState>().invoke_erasing_succeeded();
            }
            Status::ReadSuccess(read_result) => match read_result {
                // TODO: For some reason SpoolV1 tags have precedence, not sure why.
                // Probably becaue of the use if their internal written ID in case such exist, not sure it's good
                // Need to remember the reason
                spool_tag::ReadResult::TagInStore { uid } => {
                    // Handling of tag in store, same as below
                    debug!("Scanned Tag which is in store");
                    let hex_tag = hex::encode_upper(uid);
                    if let Some(spool_rec) = self.store.get_spool_by_hex_tag(&hex_tag) {
                        self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Scanned);
                        self.display_filament_staging(true);
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
                    } else {
                        error!("Tag scanned as in store, not found in store");
                    }
                }
                spool_tag::ReadResult::NDEF { uid, message } => {
                    if let Some(ndef_bytes) = message {
                        match NdefMessage::decode(ndef_bytes) {
                            Ok(ndef) => {
                                for (record_num, record) in ndef.records().iter().enumerate() {
                                    debug!("NDEF record #{record_num}, type {:?}", core::str::from_utf8(record.record_type()));

                                    if core::str::from_utf8(record.record_type()) == Ok("U") {
                                        match UriPayload::try_from(record) {
                                            Ok(uri) => {
                                                info!("Scanned Tag with URI : {}", uri.full_uri());
                                                if record_num == 0 && self.process_v1_tag_read(uid, &uri.full_uri(), false) {
                                                    return;
                                                }
                                            }
                                            Err(err) => {
                                                error!("Error decoding NDEF uri {err:?}");
                                            }
                                        }
                                    }
                                }
                            }
                            Err(err) => error!("Error decoding NDEF data on tag {err:?}"),
                        }
                    }
                    // not V1 tag
                    let hex_tag = hex::encode_upper(uid);
                    // Check if it is a known tag
                    if let Some(spool_rec) = self.store.get_spool_by_hex_tag(&hex_tag) {
                        debug!("Scanned Tag which is in store");
                        // Handling of tag in store, same as above
                        self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Scanned);
                        self.display_filament_staging(true);
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
                    } else {
                        // Not known
                        // Check if some special format
                        if let Some(ndef_bytes) = message {
                            if let Ok(ndef) = NdefMessage::decode(ndef_bytes) {
                                for record in ndef.records() {
                                    if core::str::from_utf8(record.record_type()) == Ok("application/vnd.openprinttag") {
                                        let hex_tag = hex::encode_upper(uid);
                                        info!("Scanned an OpenPrintTag tag");
                                        let open_print_tag = OpenPrintTagTag::new(&hex_tag, ndef_bytes);
                                        let open_print_tag_str = serde_json::to_string(&open_print_tag).unwrap();
                                        let ui = self.ui_weak.clone();
                                        ui.unwrap().global::<crate::app::AppState>().invoke_new_definition_tag_scanned(
                                            OPENPRINTTAG_TAG_TYPE.to_shared_string(),
                                            hex_tag.into(),
                                            open_print_tag_str.into(),
                                        );
                                        return;
                                    }
                                }
                            }
                        }

                        // Unknown format, treat as an empty tag
                        let ui = self.ui_weak.unwrap();
                        let ui_app_state = ui.global::<crate::app::AppState>();
                        ui_app_state.invoke_new_tag_scanned(hex_tag.to_shared_string());
                    }
                }
                spool_tag::ReadResult::BambulabTag { uid, data } => {
                    let hex_tag = hex::encode_upper(uid);
                    if let Some(spool_rec) = self.store.get_spool_by_hex_tag(&hex_tag) {
                        self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Scanned);
                        self.display_filament_staging(true);
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
                    } else if let Some(blocks) = data {
                        let bambu_tag = BambuLabTag::new(&hex_tag, blocks);
                        let bamtu_tag_str = serde_json::to_string(&bambu_tag).unwrap();
                        let ui = self.ui_weak.clone();
                        ui.unwrap().global::<crate::app::AppState>().invoke_new_definition_tag_scanned(
                            BAMBULAB_TAG_TYPE.to_shared_string(),
                            hex_tag.into(),
                            bamtu_tag_str.into(),
                        );
                    }
                }
            },
            Status::Failure(spool_tag::Failure::TagWriteFailure(text_str)) => {
                ui.unwrap().global::<crate::app::AppState>().invoke_encoding_failure(text_str.into());
            }
            Status::Failure(spool_tag::Failure::TagEraseFailure(text_str)) => {
                ui.unwrap().global::<crate::app::AppState>().invoke_erasing_failure(text_str.into());
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

    fn is_tag_in_store(&mut self, tag_id: &[u8]) -> bool {
        self.store.exists_tag_id(tag_id)
    }
}

impl FrameworkObserver for ViewModel {
    fn on_web_config_started(&self, key: &str, mode: WebConfigMode) {
        let mode = match mode {
            WebConfigMode::AP => crate::app::WebConfigState::StartedAP,
            WebConfigMode::STA => {
                if self.app_config.borrow().missing_configs(false) {
                    crate::app::WebConfigState::StartedSTADisplayed
                } else {
                    crate::app::WebConfigState::StartedSTA
                }
            }
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
        // self.framework.borrow().check_firmware_ota();
        self.ui_ota_check_firmwares();
    }

    fn on_wifi_sta_disconnected(&self) {
        info!("WiFi disconnected");
    }

    fn on_ota_start(&mut self) {
        self.ui_weak.unwrap().global::<crate::app::FrameworkState>().invoke_ota_started();
    }

    fn on_ota_status(&mut self, text: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_ota_status(SharedString::from(text));
    }

    fn on_ota_completed(&mut self, text: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_ota_completed(SharedString::from(text));
    }

    fn on_ota_failed(&mut self, text: &str) {
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .invoke_ota_failed(SharedString::from(text));
    }

    fn on_ota_version_available(&mut self, version: &str, newer: bool) {
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
            term_info!(&"-".repeat(62));
            term_info!("Initialization completed successfully");
            term_info!(&"-".repeat(62));
            self.ui_weak.unwrap().global::<crate::app::AppState>().invoke_initialization_completed();
        } else {
            // TODO: This event here goes to the AppState and not to Framework, think about that.
            self.ui_weak
                .unwrap()
                .global::<crate::app::AppState>()
                .invoke_boot_failed("Boot Failed\nScroll Up for Details".to_shared_string());
            term_info!(&"x".repeat(44));
            term_info!("Initialization failed - Review errors, fix, and restart");
            term_info!(&"x".repeat(44));
        }
    }
}

struct TerminalViewModel {
    ui_weak: slint::Weak<crate::app::AppWindow>,
    term_text: String,
}

impl TerminalObserver for TerminalViewModel {
    fn on_add_text(&mut self, text: &str) {
        self.term_text.push_str(text);
        let keep_from = self
            .term_text
            .match_indices('\n')
            .nth_back(50) // nth newline from the end
            .map(|(i, _)| i + 1) // start after it
            .unwrap_or(0);
        self.term_text.drain(..keep_from);

        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .set_term_text(self.term_text.to_shared_string());
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkState>()
            .set_term_text_added(text.to_shared_string());
        // self.ui_weak
        //     .unwrap()
        //     .global::<crate::app::FrameworkState>()
        //     .invoke_add_term_text(text.to_shared_string());
    }
}

pub struct SelectedPrinter {
    pub printers: Vec<Rc<RefCell<BambuPrinter>>>,
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
        let _ = self.spool_scale_model.borrow().tags_in_store(self.store.tags_in_store());
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
        self._terminal_view_model.borrow_mut().on_add_text(&text);
        // self.ui_weak
        //     .unwrap()
        //     .global::<crate::app::FrameworkState>()
        //     .invoke_add_term_text(text.into());
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

    fn on_button_pressed(&mut self, scale_weight: ScaleWeight) -> Option<bool> {
        self.update_spool_weight_from_button(scale_weight)
    }

    // note that this is from Scale (which ends up calling the GcodeAnalyzerObserver on_gcode_analysis)
    fn on_gcode_analysis(&mut self, job_number: i32, printer_index: usize, gcode_analysis: FilamentUsage) {
        shared::gcode_analysis_task::GcodeAnalyzerObserver::on_gcode_analysis(self, job_number, printer_index, gcode_analysis);
    }

    fn on_gcode_analysis_failed(&mut self, job_number: i32, printer_index: usize) {
        debug!("Gcode analysis job {job_number} from Scale failed, see scale logs for more info");
        shared::gcode_analysis_task::GcodeAnalyzerObserver::on_failed(self, job_number, printer_index);
    }

    fn on_gcode_analysis_canceled(&mut self, job_number: i32, printer_index: usize) {
        debug!("Gcode analysis job {job_number} from Scale was canceled");
        shared::gcode_analysis_task::GcodeAnalyzerObserver::on_canceled(self, job_number, printer_index);
    }

    fn on_gcode_analysis_completed(&mut self, job_number: i32, printer_index: usize) {
        debug!("Received gcode analysis job {job_number} from Scale");
        shared::gcode_analysis_task::GcodeAnalyzerObserver::on_completed(self, job_number, printer_index);
    }

    fn on_scale_version(&mut self, scale_version: &str) {
        self.scale_version = Some(scale_version.to_string());
    }

    fn on_ota_progress_update(&mut self, update: shared::scale::OtaProgressUpdate) {
        match update {
            shared::scale::OtaProgressUpdate::Start => self.on_ota_start(),
            shared::scale::OtaProgressUpdate::Status { text } => self.on_ota_status(&text),
            shared::scale::OtaProgressUpdate::Failed { text } => self.on_ota_failed(&text),
            shared::scale::OtaProgressUpdate::Completed { text } => self.on_ota_completed(&text),
            shared::scale::OtaProgressUpdate::VersionAvailable { version, newer } => self.on_ota_version_available(&version, newer),
        }
    }
}

impl StoreObserver for ViewModel {
    fn on_tag_added(&self) {
        let tags_in_store = self.store.tags_in_store();
        let _ = self.spool_scale_model.borrow().tags_in_store(tags_in_store);
    }

    fn on_tag_removed(&self) {
        let tags_in_store = self.store.tags_in_store();
        let _ = self.spool_scale_model.borrow().tags_in_store(tags_in_store);
    }
}

fn get_brand_from_text(text: &str) -> Option<&'static str> {
    let text = text.to_lowercase();
    // prioritize start with
    for brand in FILAMENT_BRAND_NAMES.lines() {
        if brand.contains(',') {
            if let Some((keyword, brand)) = brand.split_once(',') {
                if text.starts_with(&keyword.to_lowercase()) {
                    return Some(brand);
                }
            }
        } else if text.starts_with(&brand.to_lowercase()) {
            return Some(brand);
        }
    }
    // if not found continue to contains
    for brand in FILAMENT_BRAND_NAMES.lines() {
        if brand.contains(',') {
            if let Some((keyword, brand)) = brand.split_once(',') {
                if text.contains(&keyword.to_lowercase()) {
                    return Some(brand);
                }
            }
        } else if text.contains(&brand.to_lowercase()) {
            return Some(brand);
        }
    }
    None
}

fn decode_csv_field(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s[1..s.len() - 1].replace("\"\"", "\"")
    } else {
        s.to_string()
    }
}

#[derive(Debug)]
pub enum StoreStateRequest {
    StorePrintProject { printer_index: usize },
    StoreConsumeIndex { printer_index: usize, consume_store_counter: i32 },
    DeletePrintProject { printer_index: usize },
}

pub type StoreStateRequestChannel = Channel<NoopRawMutex, StoreStateRequest, 5>;

// #[embassy_executor::task] // up to two printers in parallel
pub async fn printers_scheduled_store_state_task(framework: Rc<RefCell<Framework>>, view_model: Rc<RefCell<ViewModel>>, store: Rc<Store>) {
    info!("store_state_task started");
    {
        let file_store = framework.borrow().file_store();
        let file_store = file_store.lock().await;
        if !file_store.card_installed {
            term_info!("SDCard not installed, won't restore state on restart");
            return;
        }
    }
    let num_of_printers = view_model.borrow().bambu_printer_model.printers.len();
    term_info!("Restoring printer(s) state");
    for printer_index in 0..num_of_printers {
        let printer = view_model.borrow().bambu_printer_model.printers[printer_index].clone();
        if printer.borrow().printer_name().to_lowercase() == "simulator" {
            info!("[{}] Printer Simulator - skipping restore state", printer.borrow().printer_number);
            continue;
        }
        if let Err(err) = BambuPrinter::load_printer_state(&framework, &printer, &store).await {
            view_model
                .borrow()
                .message_box("Restore Print State Notice", &err, "", crate::app::StatusType::Error, 0);
        }
        info!("[{}] Checking for print project resume state", printer.borrow().printer_number);
        match BambuPrinter::load_print_project_state(&framework, &printer).await {
            Ok(found_print_project_state) => {
                if !found_print_project_state {
                    info!("[{}] Print project resume state not found", printer.borrow().printer_number);
                    let _ = BambuPrinter::delete_print_project_state(&framework, &printer).await;
                }
            }
            Err(err) => {
                error!("{err} - Print tracking can't be resumed");
                let _ = BambuPrinter::delete_print_project_state(&framework, &printer).await;
                view_model.borrow().message_box(
                    "Restore Running Print State Notice",
                    &err,
                    "Print Tracking Can't be Resumed",
                    crate::app::StatusType::Error,
                    0,
                );
            }
        }

        let current_selected_printer = view_model.borrow().bambu_printer_model.index;
        if printer_index == current_selected_printer {
            view_model.borrow().update_ui_from_printer(&printer.borrow());
        }
    }

    let mut printer_index = 0;
    let delay_time = max(1000u64, (3000 / num_of_printers) as u64); // want every printer to save every 3 seconds, and not all together
    let store_state_request_channel = view_model.borrow().store_state_request_channel.clone();
    let receiver = store_state_request_channel.receiver();
    loop {
        // Timer::after_millis(delay_time).await;
        match with_timeout(Duration::from_millis(delay_time), receiver.receive()).await {
            Ok(request) => match request {
                StoreStateRequest::StorePrintProject { printer_index } => {
                    let printer = view_model.borrow().bambu_printer_model.printers[printer_index].clone();
                    if let Err(err) = BambuPrinter::store_print_project_state(&framework, &printer).await {
                        view_model.borrow().message_box(
                            "Print Tracking Notice",
                            &err,
                            "SpoolEase Will Not be Able to Resume Tracking if Restarted",
                            crate::app::StatusType::Error,
                            0,
                        );
                        let _ = BambuPrinter::delete_print_project_state(&framework, &printer).await;
                    }
                }
                StoreStateRequest::StoreConsumeIndex {
                    printer_index,
                    consume_store_counter,
                } => {
                    let printer = view_model.borrow().bambu_printer_model.printers[printer_index].clone();
                    if let Err(err) = BambuPrinter::store_consume_index_state(&framework, &printer, consume_store_counter).await {
                        view_model.borrow().message_box(
                            "Print Tracking Notice",
                            &err,
                            "If This Error Repeats, SpoolEase Will Not be Able to Resume Tracking if Restarted",
                            crate::app::StatusType::Error,
                            0,
                        );
                    }
                }
                StoreStateRequest::DeletePrintProject { printer_index } => {
                    let printer = view_model.borrow().bambu_printer_model.printers[printer_index].clone();
                    let _ = BambuPrinter::delete_print_project_state(&framework, &printer).await;
                }
            },
            Err(_) => {
                // Time expired
                if printer_index < num_of_printers {
                    let printer = view_model.borrow().bambu_printer_model.printers[printer_index].clone();
                    let num_retries = 3;
                    for retry in 1..=num_retries {
                        match BambuPrinter::store_printer_state(&framework, &printer, &view_model).await {
                            Ok(_) => break,
                            Err(err) => {
                                if retry == num_retries {
                                    view_model.borrow().message_box(
                                        "State Store Error",
                                        "Failed All Retries Storing State",
                                        "Please report on Github/Discord !!!",
                                        crate::app::StatusType::Error,
                                        0,
                                    );
                                    error!(
                                        "[{}] Failed all retries trying to store printer restart state : {err}",
                                        printer.borrow().printer_number
                                    );
                                }
                            }
                        }
                    }
                }
                printer_index += 1;
                if printer_index >= num_of_printers {
                    printer_index = 0;
                }
            }
        }
    }
}

// #[embassy_executor::task]
pub async fn store_printers_consume(view_model: Rc<RefCell<ViewModel>>) {
    info!("store_printers_consume task started");
    let store = view_model.borrow().store.clone();
    Timer::after_secs(10).await;
    loop {
        if store.is_available() {
            break;
        }
        Timer::after_secs(1).await;
    }
    if !store.is_available() {
        warn!("Store is not available in store_printer_consume_task");
        return;
    }
    //TODO: test CsvDB is available
    let num_of_printers = view_model.borrow().bambu_printer_model.printers.len();
    loop {
        for printer_index in 0..num_of_printers {
            let printer = view_model.borrow().bambu_printer_model.printers[printer_index].clone();
            let num_of_trays = printer.borrow().ams_trays().len();
            for tray_id in 0..num_of_trays {
                let spool_id;
                let consumed_during_print;
                let consumed_during_print_saved;
                {
                    let printer_borrow = printer.borrow();
                    let tray = &printer_borrow.ams_trays()[tray_id];
                    spool_id = if let Some(spool_id) = &tray.meta_info.spool_id {
                        spool_id.clone()
                    } else {
                        continue;
                    };
                    consumed_during_print = tray.meta_info.consumed_since_load;
                    if consumed_during_print == 0.0 {
                        continue;
                    }
                    consumed_during_print_saved = tray.meta_info.consumed_since_load_saved;
                    if consumed_during_print_saved == consumed_during_print {
                        continue;
                    }
                }
                let store = view_model.borrow().store.clone();
                if let Some(mut spool_rec) = store.get_spool_by_id(&spool_id) {
                    let consumption_to_add_save = consumed_during_print - consumed_during_print_saved;
                    spool_rec.consumed_since_add += consumption_to_add_save;
                    spool_rec.consumed_since_weight += consumption_to_add_save;
                    info!(
                        "Increase spool {} consumption by {:2}g to total so far {:2}g and since last weight to {:2}g",
                        spool_id, consumption_to_add_save, spool_rec.consumed_since_add, spool_rec.consumed_since_weight
                    );
                    match store.update_spool(spool_rec, None).await {
                        Ok(_) => {
                            // update saved in tray
                            let mut printer_borrow = printer.borrow_mut();
                            printer_borrow.update_ams_tray(tray_id, |tray| {
                                tray.meta_info.consumed_since_load_saved = tray.meta_info.consumed_since_load
                            });
                        }
                        Err(err) => {
                            error!("Error updating consumption of spool {spool_id} : {err}");
                        }
                    }
                } else {
                    error!("While updating consume data spool_id not found");
                }
            }
        }
        Timer::after_secs(1).await;
    }
}

impl GcodeAnalyzerObserver for ViewModel {
    fn on_gcode_analysis(&mut self, job_number: i32, printer_index: usize, filament_usage: FilamentUsage) {
        if let Some(printer) = self.bambu_printer_model.printers.get(printer_index) {
            let mut printer_borrow = printer.borrow_mut();
            printer_borrow.set_gcode_analysis(job_number, filament_usage);
        }
    }

    fn on_canceled(&mut self, job_number: i32, printer_index: usize) {
        if let Some(printer) = self.bambu_printer_model.printers.get(printer_index) {
            let printer_borrow = printer.borrow();
            let printer_log_id = printer_borrow.printer_number;
            info!("[{printer_log_id}] Gcode analysis job {job_number} canceled before completion (print canceled?)");
        }
        self.gcode_jobs.retain(|job| job.job_number != job_number);
        self.try_dispatch_next_gcode_job();
    }

    fn on_failed(&mut self, job_number: i32, printer_index: usize) {
        if let Some(printer) = self.bambu_printer_model.printers.get(printer_index) {
            let printer_borrow = printer.borrow();
            let printer_log_id = printer_borrow.printer_number;
            error!("[{printer_log_id}] Gcode analysis job {job_number} failed (exact error above?)");
        }
        self.gcode_jobs.retain(|job| job.job_number != job_number);
        self.try_dispatch_next_gcode_job();
        //TODO: Need to notify so it won't try to process anything? waste of work
    }

    fn on_completed(&mut self, job_number: i32, printer_index: usize) {
        if let Some(printer) = self.bambu_printer_model.printers.get(printer_index) {
            let printer_borrow = printer.borrow();
            let printer_log_id = printer_borrow.printer_number;
            info!("[{printer_log_id}] Gcode analysis job {job_number} completed successfuly");
        }
        self.gcode_jobs.retain(|job| job.job_number != job_number);
        self.try_dispatch_next_gcode_job();
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum GcodeJobLocation {
    Console,
    Scale,
    Pending,
}

#[allow(dead_code)]
#[derive(Debug)]
struct GcodeJob {
    job_number: i32,
    job_location: GcodeJobLocation,
    tls_slots: usize,
    analysis_request: Option<GcodeAnalysisRequest>,
}

#[derive(Debug, Clone)]
enum AppAsyncTaskRequest {
    ProcessV1TagRead {
        tag_id: String,
        tag: String,
        final_step: bool,
    },
    LinkTagToSpool {
        tag_id: String,
        tag_type: String,
        spool_id: String,
        final_step: bool,
    },
    UnLinkSpoolFromTag {
        spool_id: String,
    },
    SetStagingRecExt {},
    SetSpoolWeight {
        spool_id: String,
        weight_current: i32,
        weight_new: i32,
        final_step: bool,
        from_button: bool,
    },
    UpdateSpoolRec {
        spool_rec: SpoolRecord,
    },
    ConfigureTrayWithSpool {
        tray_id: i32,
        spool_id: String,
    },
    ImportDefinitionTagToInventory {
        tag_definition_type: String,
        tag_definition_info: String,
        empty_spool_weight: i32,
    },
}

type AppAsyncTasksChannel = Channel<NoopRawMutex, AppAsyncTaskRequest, 5>;

pub async fn app_async_task(view_model: Rc<RefCell<ViewModel>>) {
    info!("Main application async task started");

    let store = view_model.borrow().store.clone();
    while !store.is_available() {
        Timer::after_millis(100).await;
    }

    let channel = {
        let view_model_borrow = view_model.borrow();
        view_model_borrow.app_async_tasks_channel.clone()
    };
    let requests = channel.receiver();

    loop {
        match requests.receive().await {
            AppAsyncTaskRequest::ProcessV1TagRead { tag_id, tag, final_step } => {
                ViewModel::process_v1_tag_read_async(view_model.clone(), tag_id, tag, final_step).await
            }
            AppAsyncTaskRequest::LinkTagToSpool {
                tag_id,
                tag_type,
                spool_id,
                final_step,
            } => ViewModel::link_tag_to_spool_id_async(view_model.clone(), tag_id, tag_type, spool_id, final_step).await,
            AppAsyncTaskRequest::UnLinkSpoolFromTag { spool_id } => ViewModel::unlink_spool_from_tag_async(view_model.clone(), spool_id).await,
            AppAsyncTaskRequest::SetStagingRecExt {} => ViewModel::set_staging_rec_ext_async(view_model.clone()).await,
            AppAsyncTaskRequest::SetSpoolWeight {
                spool_id,
                weight_current,
                weight_new,
                final_step,
                from_button,
            } => ViewModel::set_spool_weight_async(view_model.clone(), spool_id, weight_current, weight_new, final_step, from_button).await,
            AppAsyncTaskRequest::UpdateSpoolRec { spool_rec } => ViewModel::update_spool_rec_async(view_model.clone(), spool_rec).await,
            AppAsyncTaskRequest::ConfigureTrayWithSpool { tray_id, spool_id } => {
                ViewModel::configure_tray_with_spool_async(view_model.clone(), tray_id, spool_id).await
            }
            AppAsyncTaskRequest::ImportDefinitionTagToInventory {
                tag_definition_type,
                tag_definition_info,
                empty_spool_weight,
            } => {
                ViewModel::import_definition_tag_to_inventory_async(view_model.clone(), tag_definition_type, tag_definition_info, empty_spool_weight)
                    .await
            }
        }
    }
}
