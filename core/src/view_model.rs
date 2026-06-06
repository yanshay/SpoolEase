use core::cell::RefCell;
use core::cmp::max;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::{
    format,
    rc::{Rc, Weak},
    string::ToString,
    vec,
    vec::Vec,
};
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer, with_timeout};
use embedded_hal_bus::spi::ExclusiveDevice;
use hashbrown::HashMap;
use ndef_rs::NdefMessage;
use serde::{Deserialize, Serialize};
use shared::settings::{
    OTA_DOMAIN_DEBUG, OTA_DOMAIN_STABLE, OTA_DOMAIN_UNSTABLE, OTA_TLS_CERTIFICATE, SCALE_DEBUG_OTA_PATH, SCALE_STABLE_OTA_PATH,
    SCALE_UNSTABLE_OTA_PATH,
};
use shared::types::AppOtaTrain;
use shared::utils::channel_send;
use slint::{ComponentHandle, Model, SharedString, ToSharedString};

use framework::prelude::*;
use framework::{
    framework::{FrameworkObserver, WebConfigMode},
    terminal::{self, TerminalObserver, term_mut},
};

use crate::api_types::{ApiPrinterSlot, ApiPrinterSlotGroup, ApiPrinterSlotsPrinter, ApiPrinterSlotsResponse};
use crate::app::{UiFilament, UiSlot, UiSlotDisplay, UiSlotGroup, UiSlotGroupKind, UiSlotState, UiSpoolRecord, UiSpoolRecordDisplay};
use crate::app_config::{BAMBU_COLOR_NAMES, BASE_FILAMENTS, BambuPrinterConfig, FILAMENT_BRAND_NAMES, MATERIALS, PrinterDriverConfig};
use crate::app_ota::{AppOtaProduct, AppOtaRequest, AppOtaRequestChannel, app_ota_task};
use crate::bambu::bambu_api::{GcodeState, PrintCommand as BambuPrintCommand};
use crate::bambu::driver_specific::{
    BambuAddPressureAdvance, BambuDriverCommand, BambuDriverQuery, BambuDriverQueryResult, BambuPressureAdvanceEntry,
};
use crate::filament_staging::StagingOrigin;
use crate::printer::{
    self as printer_domain, DriverSpecificCommand, DriverSpecificQuery, DriverSpecificQueryResult, FilamentTemps, MaterialSlotPresenceChange,
    MaterialSlotPresenceChangeKind, PrintControlCommand, PrinterChange, PrinterCommand, PrinterEvent, PrinterEventKind, PrinterId,
    PrinterRuntimePersistenceRequestChannel, PrinterRuntimePersistenceRequestKind, SlotAssignMode, SlotId, manager::PrinterManager,
};
use crate::settings::{DISPLAY_HEIGHT_PX, DISPLAY_WIDTH_PX, OTA_TOML_FILENAME};
use crate::spool_record::{FullSpoolRecord, OriginData, SpoolRecord, SpoolRecordExt};
use crate::spool_scale::{self, ScaleWeight, SpoolScaleObserver};
use crate::ssdp::{SSDPPubSubChannel, ssdp_task};
use crate::store::{Store, StoreObserver, store_safe_time_now};

use crate::tag_standards::{BAMBULAB_TAG_TYPE, BambuLabTag, OPENPRINTTAG_TAG_TYPE, OpenPrintTagTag};
use crate::types::FilamentSupInfo;
use crate::{app_config::AppConfig, bambu, filament_staging::FilamentStaging};
use shared::spool_tag::{self, SpoolTagObserver, Status, TAG_PLACEHOLDER};

#[allow(dead_code)]
const EXTRA_DEBUG: bool = false;

const TRAYS_SPACING: u32 = 5; // from app.slint AppConsts
const TITLE_CHECKERBOARD_WIDTH: u32 = DISPLAY_WIDTH_PX;
const TITLE_CHECKERBOARD_HEIGHT: u32 = 40;
const TITLE_CHECKER_CELL_W: u32 = 8;
const TITLE_CHECKER_CELL_H: u32 = 8;
const COLOR_CHECKERBOARD_WIDTH: u32 = (DISPLAY_WIDTH_PX - 4 * TRAYS_SPACING) / 5;
const COLOR_CHECKERBOARD_HEIGHT: u32 = if DISPLAY_HEIGHT_PX == 480 { 210 } else { 160 };
const COLOR_CHECKER_CELL_W: u32 = 6;
const COLOR_CHECKER_CELL_H: u32 = 6;
const AMS_COLOR_CHECKERBOARD_WIDTH: u32 = (DISPLAY_WIDTH_PX - 4 * TRAYS_SPACING) / 5;
const AMS_COLOR_CHECKERBOARD_HEIGHT: u32 = 40;
const AMS_COLOR_CHECKER_CELL_W: u32 = 6;
const AMS_COLOR_CHECKER_CELL_H: u32 = 6;
const TITLE_CHECKER_LIGHT: (u8, u8, u8, u8) = (255, 255, 255, 255);
const TITLE_CHECKER_DARK: (u8, u8, u8, u8) = (204, 204, 204, 255);

#[allow(unused_macros)]
macro_rules! debugex {
    ($($t:tt)*) => {
        if EXTRA_DEBUG {
            debug!($($t)*);
        }
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PrintState {
    Unknown,
    Idle,
    Prepare,
    Slicing,
    Running,
    Pause,
    Finish,
    Failed,
}
impl From<GcodeState> for PrintState {
    fn from(value: GcodeState) -> Self {
        match value {
            GcodeState::Unknown => PrintState::Unknown,
            GcodeState::IDLE => PrintState::Idle,
            GcodeState::SLICING => PrintState::Slicing,
            GcodeState::PREPARE => PrintState::Prepare,
            GcodeState::RUNNING => PrintState::Running,
            GcodeState::FINISH => PrintState::Finish,
            GcodeState::FAILED => PrintState::Failed,
            GcodeState::PAUSE => PrintState::Pause,
            GcodeState::Unsupported => PrintState::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpoolsSlotsKind {
    Ams,
    Ext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotSet {
    id: String,
    kind: SpoolsSlotsKind,
    name: String,
    short_name: String,
    driver_info: printer_domain::SlotGroupDriverInfo,
    slots: Vec<String>,
    temp: Option<f32>,
    humidity: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotSetDisplayGroup {
    id: String,
    name: String,
    short_name: String,
    slot_set_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrinterInfo {
    printer_name: String,
    printer_serial: String,
    connected: bool,
    num_ams: Option<u32>,
    print_state: PrintState,
    progress_percent: Option<i32>,
    remain_secs: Option<i32>,
    print_name: Option<String>,
    layer: Option<i32>,
    num_layers: Option<i32>,
    stage: Option<i32>, // using code to avoid large texts in binary, if need to use in UI then need to swicth to text
    print_error: Option<i32>,
    hms_errors: Vec<(i32, i32)>,
    num_extruders: u32,
    slots_sets: Vec<SlotSet>,
    slot_set_display_groups: Vec<SlotSetDisplayGroup>,
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
    printer_manager: RefCell<PrinterManager>,
    spool_tag_model: Rc<RefCell<spool_tag::SpoolTag>>,
    spool_scale_model: Rc<RefCell<spool_scale::SpoolScale>>,
    pub filament_staging: Rc<RefCell<FilamentStaging>>,
    pub store: Rc<Store>,
    ui_selected_printer_id: Option<PrinterId>,
    ssdp_pub_sub: &'static SSDPPubSubChannel,
    app_async_tasks_channel: Rc<AppAsyncTasksChannel>,
    pub recently_added_spool_id: Option<String>,
    runtime_persistence_request_channel: Rc<PrinterRuntimePersistenceRequestChannel>,
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

enum StorageRackValue {
    Bays,
    Shelves,
    Positions,
    Containers,
}

impl ViewModel {
    fn normalize_hex_color(hex: &str) -> &str {
        hex.trim().trim_start_matches('#')
    }

    fn create_checkerboard_image(width: u32, height: u32, cell_w: u32, cell_h: u32) -> slint::Image {
        let width_usize = width as usize;
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(width, height);
        let pixels = buffer.make_mut_slice();

        for y in 0..height as usize {
            for x in 0..width as usize {
                let x_block = x / cell_w as usize;
                let y_block = y / cell_h as usize;
                let use_light = (x_block + y_block).is_multiple_of(2);
                let (r, g, b, a) = if use_light { TITLE_CHECKER_LIGHT } else { TITLE_CHECKER_DARK };
                pixels[y * width_usize + x] = slint::Rgba8Pixel { r, g, b, a };
            }
        }

        slint::Image::from_rgba8(buffer)
    }

    fn create_title_checkerboard_image() -> slint::Image {
        Self::create_checkerboard_image(
            TITLE_CHECKERBOARD_WIDTH,
            TITLE_CHECKERBOARD_HEIGHT,
            TITLE_CHECKER_CELL_W,
            TITLE_CHECKER_CELL_H,
        )
    }

    fn create_color_checkerboard_image() -> slint::Image {
        Self::create_checkerboard_image(
            COLOR_CHECKERBOARD_WIDTH,
            COLOR_CHECKERBOARD_HEIGHT,
            COLOR_CHECKER_CELL_W,
            COLOR_CHECKER_CELL_H,
        )
    }

    fn create_ams_color_checkerboard_image() -> slint::Image {
        Self::create_checkerboard_image(
            AMS_COLOR_CHECKERBOARD_WIDTH,
            AMS_COLOR_CHECKERBOARD_HEIGHT,
            AMS_COLOR_CHECKER_CELL_W,
            AMS_COLOR_CHECKER_CELL_H,
        )
    }

    fn create_circular_checkerboard_image(diameter: u32, offset_x: i32, offset_y: i32) -> slint::Image {
        let width_usize = diameter as usize;
        let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(diameter, diameter);
        let pixels = buffer.make_mut_slice();

        let radius = diameter as f32 / 2.0;
        let center = radius;
        let checker_cell_w = COLOR_CHECKER_CELL_W as i32;
        let checker_cell_h = COLOR_CHECKER_CELL_H as i32;

        for y in 0..diameter as usize {
            for x in 0..diameter as usize {
                let fx = x as f32 + 0.5;
                let fy = y as f32 + 0.5;
                let dx = fx - center;
                let dy = fy - center;
                let inside_circle = (dx * dx + dy * dy) <= (radius * radius);

                let pixel = if inside_circle {
                    let global_x = offset_x + x as i32;
                    let global_y = offset_y + y as i32;
                    let x_block = global_x.div_euclid(checker_cell_w);
                    let y_block = global_y.div_euclid(checker_cell_h);
                    let use_light = ((x_block + y_block) % 2) == 0;
                    let (r, g, b, a) = if use_light { TITLE_CHECKER_LIGHT } else { TITLE_CHECKER_DARK };
                    slint::Rgba8Pixel { r, g, b, a }
                } else {
                    slint::Rgba8Pixel { r: 0, g: 0, b: 0, a: 0 }
                };

                pixels[y * width_usize + x] = pixel;
            }
        }

        slint::Image::from_rgba8(buffer)
    }

    fn rgba_hex_to_slint_color(hex: &str) -> Option<slint::Color> {
        let hex = Self::normalize_hex_color(hex);
        if hex.len() < 8 {
            return None;
        }
        let rgba = u32::from_str_radix(&hex[..8], 16).ok()?;
        let r = (rgba >> 24) & 0xFF;
        let g = (rgba >> 16) & 0xFF;
        let b = (rgba >> 8) & 0xFF;
        let a = rgba & 0xFF;
        let argb = (a << 24) | (r << 16) | (g << 8) | b;
        Some(slint::Color::from_argb_encoded(argb))
    }

    fn ui_colors_from_color_codes(color_codes: &[String]) -> Vec<slint::Color> {
        color_codes
            .iter()
            .flat_map(|color_code| color_code.split(';'))
            .filter_map(Self::rgb_or_rgba_hex_to_slint_color)
            .collect()
    }

    fn rgb_or_rgba_hex_to_slint_color(hex: &str) -> Option<slint::Color> {
        let hex = Self::normalize_hex_color(hex);
        if hex.len() >= 8 {
            return Self::rgba_hex_to_slint_color(hex);
        }
        if hex.len() >= 6 {
            let rgb = u32::from_str_radix(&hex[..6], 16).ok()?;
            return Some(slint::Color::from_argb_encoded(0xFF000000 | rgb));
        }
        None
    }

    fn rgba_hex_has_alpha(hex: &str) -> bool {
        let hex = Self::normalize_hex_color(hex);
        if hex.len() < 8 {
            return false;
        }
        !hex[6..8].eq_ignore_ascii_case("FF")
    }

    fn color_codes_have_alpha(color_codes: &[String]) -> bool {
        color_codes
            .iter()
            .flat_map(|color_code| color_code.split(';'))
            .any(Self::rgba_hex_has_alpha)
    }

    pub fn new(
        // Framework
        stack: Stack<'static>,
        ui_weak: slint::Weak<crate::app::AppWindow>,
        framework: Rc<RefCell<Framework>>,
        // Application
        app_config: Rc<RefCell<AppConfig>>,
        spi_device: Option<
            ExclusiveDevice<esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>, esp_hal::gpio::Output<'static>, embassy_time::Delay>,
        >,
        irq: Option<esp_hal::gpio::Input<'static>>,
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

        let printer_manager = RefCell::new(PrinterManager::new());

        // Initialize SpoolTag
        let spool_tag_model = if let (Some(spi_device), Some(irq)) = (spi_device, irq) {
            spool_tag::init(spi_device, irq, 1000, spawner)
        } else {
            spool_tag::init_disabled()
        };

        // Initialize ssdp
        let ssdp_pub_sub = mk_static!(SSDPPubSubChannel, SSDPPubSubChannel::new());
        spawner.spawn_heap(ssdp_task(framework.clone(), ssdp_pub_sub)).ok();

        // Initialize store
        let store = Store::new(framework.clone());

        // Initialize spool_scale_model
        let spool_scale_model = crate::spool_scale::init(framework.clone(), app_config.clone(), stack, spawner, ssdp_pub_sub);

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
            printer_manager,
            spool_tag_model: spool_tag_model.clone(),
            spool_scale_model: spool_scale_model.clone(),
            app_config: app_config.clone(),
            filament_staging: Rc::new(RefCell::new(FilamentStaging::new(store.clone()))),
            store,
            ui_selected_printer_id: None,
            ssdp_pub_sub,
            app_async_tasks_channel,
            recently_added_spool_id: None,
            runtime_persistence_request_channel: Rc::new(PrinterRuntimePersistenceRequestChannel::new()),
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
        // Subscribe before starting the store task so startup errors can be shown immediately.
        let trait_for_store_rc: Rc<RefCell<dyn StoreObserver>> = self.view_model.as_ref().unwrap().clone();
        let trait_for_store_weak: Weak<RefCell<dyn StoreObserver>> = Rc::downgrade(&trait_for_store_rc);
        self.store.subscribe(trait_for_store_weak);

        self.store.start(self.view_model.clone().unwrap());

        // Initialize Printers ///////////////////////////

        let mut default_printer_ui_index = None;
        let mut available_printers: Vec<crate::app::Printer> = Vec::new();

        let configured_printers = self.app_config.borrow().configured_printers.printers.clone();
        let configured_default_printer_id = self.app_config.borrow().configured_default_printer.printer_id.clone();
        let no_configured_printers = configured_printers.is_empty();

        for printer_config in configured_printers.iter() {
            let manager_index = self.printer_manager.borrow().len();
            let printer_number = manager_index + 1;
            if printer_number > 5 {
                term_info!("Printers limit reached - max five printers supported");
                break;
            }

            match &printer_config.driver {
                PrinterDriverConfig::Bambu(bambu_config) => {
                    match bambu::init(
                        self.framework.clone(),
                        printer_number,
                        &printer_config.name,
                        bambu_config,
                        self.app_config.clone(),
                        self.ssdp_pub_sub,
                        self.runtime_persistence_request_channel.clone(),
                    ) {
                        Ok(bambu_printer_model) => {
                            self.printer_manager.borrow_mut().add_bambu_printer(bambu_printer_model.clone());
                            let printer_id = BambuPrinterConfig::printer_id_for_serial(&bambu_printer_model.borrow().printer_serial);
                            if default_printer_ui_index.is_none() && Some(&printer_id.0) == configured_default_printer_id.as_ref() {
                                default_printer_ui_index = Some(available_printers.len());
                            }
                            let capabilities = self.printer_manager.borrow().capabilities_at(manager_index).unwrap_or_default();
                            let printer = crate::app::Printer {
                                id: printer_id.0.to_shared_string(),
                                can_assign_slot: capabilities.material_slot_assign,
                                can_set_spool_id: capabilities.material_slot_set_spool_id,
                                can_clear_slot: capabilities.material_slot_clear,
                                can_unassign_slot: capabilities.material_slot_unassign_spool,
                                connected: false,
                                name: bambu_printer_model.borrow().printer_selector_name.to_shared_string(),
                                kind: crate::app::UiPrinterKind::Bambu,
                            };
                            available_printers.push(printer);

                            // notification from printer on events, should be treated for all printers,
                            // but selected printer should be considered as to what to update in the UI
                            if let Some(view_model_rc) = &self.view_model {
                                let trait_for_printer_rc: Rc<RefCell<dyn printer_domain::PrinterObserver>> = view_model_rc.clone();
                                let trait_for_printer_weak: Weak<RefCell<dyn printer_domain::PrinterObserver>> = Rc::downgrade(&trait_for_printer_rc);
                                if let Err(err) = self.printer_manager.borrow_mut().subscribe_at(manager_index, trait_for_printer_weak) {
                                    error!("Failed to subscribe generic printer observer: {err:?}");
                                }
                            }
                            if let Err(err) = self.printer_manager.borrow_mut().start_at(manager_index, self.framework.clone()) {
                                error!("Failed to start generic printer runtime: {err:?}");
                            }
                        }
                        Err(e) => {
                            term_info!("[{}] Error initializing printer: {}", printer_number, e);
                        }
                    }
                }
                PrinterDriverConfig::Fake(fake_config) => {
                    let Ok(printer_id) = fake_config.printer_id() else {
                        term_info!("[{}] Skipping fake printer with invalid config", printer_number);
                        continue;
                    };
                    self.printer_manager
                        .borrow_mut()
                        .add_fake_printer(printer_config.name.clone(), fake_config);
                    if let Some(view_model_rc) = &self.view_model {
                        let trait_for_printer_rc: Rc<RefCell<dyn printer_domain::PrinterObserver>> = view_model_rc.clone();
                        let trait_for_printer_weak: Weak<RefCell<dyn printer_domain::PrinterObserver>> = Rc::downgrade(&trait_for_printer_rc);
                        if let Err(err) = self.printer_manager.borrow_mut().subscribe_at(manager_index, trait_for_printer_weak) {
                            error!("Failed to subscribe generic printer observer: {err:?}");
                        }
                    }
                    if let Err(err) = self.printer_manager.borrow_mut().start_at(manager_index, self.framework.clone()) {
                        error!("Failed to start generic printer runtime: {err:?}");
                    }
                    let capabilities = self.printer_manager.borrow().capabilities_at(manager_index).unwrap_or_default();
                    if default_printer_ui_index.is_none() && Some(&printer_id.0) == configured_default_printer_id.as_ref() {
                        default_printer_ui_index = Some(available_printers.len());
                    }
                    available_printers.push(crate::app::Printer {
                        id: printer_id.0.to_shared_string(),
                        can_assign_slot: capabilities.material_slot_assign,
                        can_set_spool_id: capabilities.material_slot_set_spool_id,
                        can_clear_slot: capabilities.material_slot_clear,
                        can_unassign_slot: capabilities.material_slot_unassign_spool,
                        connected: true,
                        name: crate::app_config::FakePrinterConfig::configured_display_name(&printer_config.name).to_shared_string(),
                        kind: crate::app::UiPrinterKind::Fake,
                    });
                }
            }
        }

        if default_printer_ui_index.is_none() && !available_printers.is_empty() {
            default_printer_ui_index = Some(0);
        }

        let ui = self.ui_weak.unwrap();
        let ui_app_backend = ui.global::<crate::app::AppBackend>();
        let ui_app_state = ui.global::<crate::app::AppState>();
        let default_printer_id = default_printer_ui_index
            .and_then(|index| available_printers.get(index))
            .map(|printer| PrinterId::new(printer.id.to_string()));
        self.ui_selected_printer_id = default_printer_id.clone();
        if let Some(default_printer_id) = default_printer_id {
            let manager_index = self.printer_manager.borrow().index_by_id(&default_printer_id);
            if let Some(manager_index) = manager_index {
                let _ = self.printer_manager.borrow_mut().set_selected_index(manager_index);
            }
        }

        ui_app_state.set_title_checkerboard_bg(Self::create_title_checkerboard_image());
        ui_app_state.set_color_checkerboard_bg(Self::create_color_checkerboard_image());
        ui_app_state.set_ams_color_checkerboard_bg(Self::create_ams_color_checkerboard_image());

        if no_configured_printers || available_printers.is_empty() {
            ui_app_state.set_no_printers_configured(true);
        }

        let default_printer = default_printer_ui_index.map(|index| index as i32).unwrap_or(-1);
        let available_printers = slint::ModelRc::new(slint::VecModel::from(available_printers));
        ui_app_state.invoke_set_printers_info(available_printers, default_printer);
        ui_app_state.invoke_set_curr_printer(default_printer);
        self.update_slot_groups_from_selected_printer();

        let moved_ui = self.ui_weak.clone();
        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        // this select_printer handler CAN'T depend on printer because then it would need to change itself while running
        ui_app_backend.on_select_printer(move |selected_printer_id| {
            // First stored UI for this printer for when we switch back to it
            Self::perform_select_printer(moved_ui.clone(), moved_view_model.clone(), selected_printer_id.to_string());
        });

        if self.printer_manager.borrow().len() > 0 {
            self.framework
                .borrow()
                .spawner
                .spawn_heap(printers_scheduled_store_state_task(
                    self.framework.clone(),
                    self.view_model.clone().unwrap(),
                    self.store.clone(),
                ))
                .ok();
        }

        let has_consumption_tracking = {
            let printer_manager = self.printer_manager.borrow();
            (0..printer_manager.len()).any(|printer_index| printer_manager.capabilities_at(printer_index).unwrap_or_default().consumption_tracking)
        };

        if has_consumption_tracking {
            self.framework
                .borrow()
                .spawner
                .spawn_heap(store_printers_consume(self.view_model.clone().unwrap()))
                .ok();
        }

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_link_tag_to_untagged_spool_id(move |tag_id, tag_type, spool_id, final_step| {
            let _ = moved_view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::LinkTagToSpool {
                tag_id: tag_id.into(),
                tag_type: tag_type.into(),
                spool_id: spool_id.into(),
                mode: LinkTagMode::ToUntaggedSpool,
                final_step,
            });
        });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_link_tag_to_tagged_spool_id(move |tag_id, tag_type, spool_id, final_step| {
            let _ = moved_view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::LinkTagToSpool {
                tag_id: tag_id.into(),
                tag_type: tag_type.into(),
                spool_id: spool_id.into(),
                mode: LinkTagMode::ToTaggedSpool,
                final_step,
            });
        });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_unlink_spool_from_all_tags(move |spool_id| {
            let _ = moved_view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::UnLinkSpoolTags {
                spool_id: spool_id.into(),
                mode: UnlinkTagMode::AllTags,
            });
        });

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_unlink_scanned_tag_from_spool(move |spool_id| {
            let _ = moved_view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::UnLinkSpoolTags {
                spool_id: spool_id.into(),
                mode: UnlinkTagMode::ScannedTag,
            });
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
            if let Some(spool_id) = &moved_view_model.borrow().recently_added_spool_id
                && let Some(spool_rec) = store.get_spool_by_id(spool_id)
                && !spool_rec.has_valid_tag_id()
            {
                return spool_id.to_shared_string();
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
                    let filament_sup_info = view_model_borrow.get_filament_info(&spool_rec.slicer_filament, Some(&spool_rec.material_type));
                    match spool_rec.to_tag_descriptor_s1(&filament_sup_info) {
                        Some(descriptor) => {
                            let spool_tag_borrow = view_model_borrow.spool_tag_model.borrow();
                            let spool_scale_borrow = view_model_borrow.spool_scale_model.borrow();
                            let encode_cookie = SpoolEncodeCookie {
                                spool_rec_id: spool_rec.id.clone(),
                                encode_time: spool_rec.encode_time,
                            };
                            let encode_cookie_str = serde_json::to_string(&encode_cookie).unwrap();
                            let allowed_uids: Vec<Vec<u8>> = spool_rec.linked_tag_ids().filter_map(|tag_id| hex::decode(tag_id).ok()).collect();
                            if !allowed_uids.is_empty() {
                                spool_tag_borrow.write_tag(&descriptor, Some(allowed_uids.clone()), encode_cookie_str.clone());
                                let _ = spool_scale_borrow.write_tag(&descriptor, Some(allowed_uids), encode_cookie_str);
                                true
                            } else {
                                ui.invoke_encoding_failure("Spool has no valid linked tag IDs".to_shared_string());
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
        ui_framework_state.set_display_width(DISPLAY_WIDTH_PX as f32);
        ui_framework_state.set_display_height(DISPLAY_HEIGHT_PX as f32);
        ui_framework_state.set_app_info(crate::app::AppInfo {
            name: env!("CARGO_PKG_NAME").into(),
            version: env!("CARGO_PKG_VERSION").into(),
        });

        // Register to UI (Slint) framework events (UI FrameworkBackend API's)
        let ui_framework_backend = ui.global::<crate::app::FrameworkBackend>();

        let framework = self.framework.clone();
        ui_framework_backend.on_reset_flash_wifi_credentials(move || {
            framework.borrow_mut().erase_stored_wifi_credentials();
            framework.borrow_mut().reset_device_safer(Some(Duration::from_secs(3)));
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
            framework.borrow_mut().reset_device_safer(Some(Duration::from_secs(3)));
        });

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

        let ui = self.ui_weak.unwrap();
        let ui_app_backend = ui.global::<crate::app::AppBackend>();
        let ui_app_state = ui.global::<crate::app::AppState>();

        let moved_ui = self.ui_weak.clone();
        let moved_circle_checker_cache = Rc::new(RefCell::new(HashMap::<(i32, i32, i32), slint::Image>::new()));
        let moved_expected_circle_checker_key = Rc::new(RefCell::new(None::<(i32, i32, i32)>));
        let ensure_circle_checkerboard: Rc<dyn Fn(i32, i32, i32)> = Rc::new(move |diameter, offset_x, offset_y| {
            if diameter <= 0 {
                return;
            }
            let request_key = (diameter, offset_x, offset_y);
            {
                let mut expected_key = moved_expected_circle_checker_key.borrow_mut();
                if expected_key.is_none() {
                    *expected_key = Some(request_key);
                }
                expected_key
                    .as_ref()
                    .filter(|key| **key == request_key)
                    .expect(
                        "Circle checkerboard geometry changed after first request. This is unexpected for now; update cache/flow before allowing multiple sizes/offsets.",
                    );
            }
            let maybe_cached = moved_circle_checker_cache.borrow().get(&request_key).cloned();
            let checker_image = if let Some(image) = maybe_cached {
                image
            } else {
                let diameter_u32 = diameter as u32;
                let checker_image = Self::create_circular_checkerboard_image(diameter_u32, offset_x, offset_y);
                moved_circle_checker_cache.borrow_mut().insert(request_key, checker_image.clone());
                checker_image
            };
            moved_ui
                .unwrap()
                .global::<crate::app::AppState>()
                .set_circle_checkerboard_bg(checker_image);
        });
        let ensure_circle_checkerboard_for_callback = ensure_circle_checkerboard.clone();
        ui_app_backend.on_ensure_circle_checkerboard(move |diameter, offset_x, offset_y| {
            ensure_circle_checkerboard_for_callback(diameter, offset_x, offset_y);
        });

        // Catch up in case UI computed diameter before callback registration finished.
        let initial_circle_diameter = ui_app_state.get_circle_checkerboard_diameter();
        let initial_circle_offset_x = ui_app_state.get_circle_checkerboard_offset_x();
        let initial_circle_offset_y = ui_app_state.get_circle_checkerboard_offset_y();
        if initial_circle_diameter > 0 {
            ensure_circle_checkerboard(initial_circle_diameter, initial_circle_offset_x, initial_circle_offset_y);
        }

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
        ui_app_backend.on_web_config_web_app(move || {
            // moved_app_config.borrow_mut().set_redirect_web_to_config();
            let borrowed_framework = moved_framework.borrow();
            let web_config_ip_url = &borrowed_framework.web_config_ip_url;
            let web_config_key = &borrowed_framework.web_config_key;
            let full_web_config_url = format!("{web_config_ip_url}/config#sk={web_config_key}");
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
            .on_get_slot_display(move |slot_id| moved_view_model.borrow().ui_get_slot_display(slot_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_spool_tag_count(move |spool_id| moved_view_model.borrow().ui_spool_tag_count(spool_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_staging_scanned_tag_id(move || moved_view_model.borrow().ui_staging_scanned_tag_id());

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_can_link_untagged_spool_to_tag(move |spool_id| moved_view_model.borrow().ui_can_link_untagged_spool_to_tag(spool_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_can_link_tagged_spool_to_tag(move |spool_id| moved_view_model.borrow().ui_can_link_tagged_spool_to_tag(spool_id.as_str()));

        let moved_view_model = self.view_model.clone().unwrap();
        ui_app_backend.on_import_definition_tag_to_inventory(move |tag_definition_type, tag_definition_info, empty_spool_weight, spool_is_full| {
            moved_view_model.borrow().ui_import_definition_tag_to_inventory(
                tag_definition_type.as_str(),
                tag_definition_info.as_str(),
                empty_spool_weight,
                spool_is_full,
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
            .on_erase_staging_tag(move || moved_view_model.borrow().ui_erase_staging_tag());

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::FrameworkBackend>()
            .on_term_info(move |text| moved_view_model.borrow().ui_term_info(text.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_unassign_slot(move |slot_id| moved_view_model.borrow().ui_unassign_slot(slot_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_reset_slot(move |slot_id| moved_view_model.borrow().ui_reset_slot(slot_id.as_str()));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_configure_slot_with_spool_id(move |slot_id, spool_id| {
                moved_view_model.borrow().ui_configure_slot_with_spool_id(slot_id.as_str(), &spool_id)
            });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_set_staging_to_slot(move |slot_id| moved_view_model.borrow().ui_set_staging_to_slot(slot_id.as_str()));

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

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_update_actual_location(move |spool_id, location, message| {
                moved_view_model.borrow().ui_update_actual_location(&spool_id, &location, &message)
            });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_storage_rack_count(move || moved_view_model.borrow().ui_storage_rack_count());

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_get_storage_rack_options(move || moved_view_model.borrow().ui_get_storage_rack_options());

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_storage_available_bays(move |rack_id| moved_view_model.borrow().ui_storage_rack_value(rack_id, StorageRackValue::Bays));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_storage_available_shelves(move |rack_id, _bay| moved_view_model.borrow().ui_storage_rack_value(rack_id, StorageRackValue::Shelves));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_storage_available_positions(move |rack_id, _bay, _shelf| {
                moved_view_model.borrow().ui_storage_rack_value(rack_id, StorageRackValue::Positions)
            });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_storage_available_containers(move |rack_id, _bay, _shelf, _position| {
                moved_view_model.borrow().ui_storage_rack_value(rack_id, StorageRackValue::Containers)
            });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_storage_location_str(move |rack_id, bay, shelf, position, container| {
                moved_view_model
                    .borrow()
                    .ui_storage_location_str(rack_id, bay, shelf, position, container)
            });

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_location_str_to_location(move |location_str| moved_view_model.borrow().ui_location_str_to_location(&location_str));

        let moved_view_model = self.view_model.as_ref().unwrap().clone();
        self.ui_weak
            .unwrap()
            .global::<crate::app::AppBackend>()
            .on_load_staging(move |spool_id| moved_view_model.borrow().ui_load_staging(&spool_id));
    }

    fn perform_select_printer(moved_ui: slint::Weak<crate::app::AppWindow>, moved_view_model: Rc<RefCell<ViewModel>>, selected_printer_id: String) {
        let ui = moved_ui.unwrap();
        let ui_app_state = ui.global::<crate::app::AppState>();
        let printer_id = PrinterId::new(selected_printer_id);

        let mut borrowed_view_model = moved_view_model.borrow_mut();
        let manager_index = borrowed_view_model.printer_manager.borrow().index_by_id(&printer_id);
        let Some(manager_index) = manager_index else {
            error!("Selected printer {} has no generic manager mapping", printer_id.as_str());
            return;
        };
        let selected_ui_index = borrowed_view_model.ui_index_for_printer_id(&printer_id).unwrap_or(-1);
        ui_app_state.invoke_set_curr_printer(selected_ui_index);
        borrowed_view_model.ui_selected_printer_id = Some(printer_id);
        if let Err(err) = borrowed_view_model.printer_manager.borrow_mut().set_selected_index(manager_index) {
            error!("Failed to select printer at index {manager_index}: {err:?}");
        }
        borrowed_view_model.update_slot_groups_from_selected_printer();
    }

    fn selected_manager_index(&self) -> Option<usize> {
        self.ui_selected_printer_id
            .as_ref()
            .and_then(|printer_id| self.printer_manager.borrow().index_by_id(printer_id))
    }

    fn selected_printer_snapshot(&self) -> Option<(i32, usize, printer_domain::PrinterSnapshot)> {
        let manager_index = self.selected_manager_index()?;
        let snapshot = self.printer_manager.borrow().snapshot_at(manager_index)?;
        let current_printer = self
            .ui_selected_printer_id
            .as_ref()
            .and_then(|printer_id| self.ui_index_for_printer_id(printer_id))
            .unwrap_or(-1);
        Some((current_printer, manager_index, snapshot))
    }

    fn printer_snapshot_by_id(&self, printer_id: &PrinterId) -> Option<(usize, printer_domain::PrinterSnapshot)> {
        let printer_manager = self.printer_manager.borrow();
        let manager_index = printer_manager.index_by_id(printer_id)?;
        let snapshot = printer_manager.snapshot_at(manager_index)?;
        Some((manager_index, snapshot))
    }

    fn ui_index_for_manager_index(&self, manager_index: usize) -> Option<i32> {
        let printer_id = self.printer_manager.borrow().id_at(manager_index)?;
        self.ui_index_for_printer_id(&printer_id)
    }

    fn printer_id_for_manager_index(&self, manager_index: usize) -> Option<String> {
        self.printer_manager.borrow().id_at(manager_index).map(|printer_id| printer_id.0)
    }

    fn ui_index_for_printer_id(&self, printer_id: &PrinterId) -> Option<i32> {
        let ui = self.ui_weak.unwrap();
        let ui_printers = ui.global::<crate::app::AppState>().get_available_printers();
        for index in 0..ui_printers.row_count() {
            if let Some(printer) = ui_printers.row_data(index)
                && printer.id.as_str() == printer_id.as_str()
            {
                return Some(index as i32);
            }
        }
        None
    }

    fn slot_description_from_snapshot(snapshot: &printer_domain::PrinterSnapshot, slot_id: &str) -> String {
        snapshot
            .slot_groups
            .iter()
            .flat_map(|group| group.slots.iter())
            .find(|slot| slot.id.as_str() == slot_id)
            .map(|slot| slot.display_name.clone())
            .unwrap_or_else(|| slot_id.to_string())
    }

    fn update_slot_groups_from_selected_printer(&self) {
        let selected_snapshot = self
            .ui_selected_printer_id
            .as_ref()
            .and_then(|printer_id| self.printer_snapshot_by_id(printer_id).map(|(_, snapshot)| snapshot));

        if let Some(snapshot) = &selected_snapshot {
            self.update_slot_groups_from_snapshot(snapshot);
        } else {
            self.ui_weak.unwrap().global::<crate::app::AppState>().set_slot_groups_loading(false);
            self.set_ui_slot_groups(Vec::new());
        }
    }

    fn update_slot_groups_from_snapshot(&self, snapshot: &printer_domain::PrinterSnapshot) {
        let ui = self.ui_weak.unwrap();
        let ui_app_state = ui.global::<crate::app::AppState>();
        if !snapshot.slot_groups_known {
            ui_app_state.set_slot_groups_loading(true);
            self.set_ui_slot_groups(Vec::new());
            return;
        }

        ui_app_state.set_slot_groups_loading(false);
        let groups = snapshot
            .slot_groups
            .iter()
            .map(|group| self.ui_slot_group_from_snapshot(group))
            .collect::<Vec<_>>();
        self.set_ui_slot_groups(groups);
    }

    fn set_ui_slot_groups(&self, groups: Vec<UiSlotGroup>) {
        let primary_groups = groups
            .iter()
            .filter(|group| group.kind != UiSlotGroupKind::External)
            .cloned()
            .collect::<Vec<_>>();
        let external_groups = groups
            .iter()
            .filter(|group| group.kind == UiSlotGroupKind::External)
            .cloned()
            .collect::<Vec<_>>();
        let ui = self.ui_weak.unwrap();
        let ui_app_state = ui.global::<crate::app::AppState>();

        ui_app_state.set_slot_groups(slint::ModelRc::from(Rc::new(slint::VecModel::from(groups))));
        ui_app_state.set_primary_slot_groups(slint::ModelRc::from(Rc::new(slint::VecModel::from(primary_groups))));
        ui_app_state.set_external_slot_groups(slint::ModelRc::from(Rc::new(slint::VecModel::from(external_groups))));

        if ui_app_state.get_selected_primary_slot_group() >= ui_app_state.get_primary_slot_groups().row_count() as i32 {
            ui_app_state.set_selected_primary_slot_group(0);
        }
        if ui_app_state.get_displayed_external_slot_group() >= ui_app_state.get_external_slot_groups().row_count() as i32 {
            ui_app_state.set_displayed_external_slot_group(0);
        }
    }

    fn ui_slot_group_from_snapshot(&self, group: &printer_domain::SlotGroupSnapshot) -> UiSlotGroup {
        let slots = group.slots.iter().map(|slot| self.ui_slot_from_snapshot(slot)).collect::<Vec<_>>();
        UiSlotGroup {
            id: group.id.to_shared_string(),
            kind: Self::ui_slot_group_kind(group.kind),
            name: group.name.to_shared_string(),
            short_name: group.short_name.to_shared_string(),
            slots: slint::ModelRc::from(Rc::new(slint::VecModel::from(slots))),
        }
    }

    fn ui_slot_from_snapshot(&self, slot: &printer_domain::MaterialSlotSnapshot) -> UiSlot {
        let (filament_state, filament_colors, filament_has_alpha, material) = match &slot.filament {
            printer_domain::PrinterFilament::Known(filament) => {
                let material = if filament.material_type.is_empty() {
                    filament.slicer_filament.as_str()
                } else {
                    filament.material_type.as_str()
                };
                (
                    crate::app::UiFilamentState::Known,
                    Self::non_empty_ui_colors(Self::ui_colors_from_color_codes(&filament.color_codes)),
                    Self::color_codes_have_alpha(&filament.color_codes),
                    material.to_shared_string(),
                )
            }
            printer_domain::PrinterFilament::Unknown => (
                crate::app::UiFilamentState::Unknown,
                Self::non_empty_ui_colors(Vec::new()),
                false,
                "???".to_shared_string(),
            ),
        };
        let (spool_colors, spool_has_alpha) = slot
            .spool_id
            .as_ref()
            .and_then(|spool_id| self.store.get_spool_by_id(spool_id))
            .map(|spool| {
                (
                    Self::ui_colors_from_color_codes(&spool.color_code),
                    Self::color_codes_have_alpha(&spool.color_code),
                )
            })
            .unwrap_or_default();

        UiSlot {
            id: slot.id.as_str().to_shared_string(),
            name: if slot.short_name.is_empty() {
                slot.display_name.as_str()
            } else {
                slot.short_name.as_str()
            }
            .to_shared_string(),
            state: Self::ui_slot_state_from_slot_state(slot.state),
            state_label: Self::slot_state_label(slot.state).into(),
            filament: UiFilament {
                state: filament_state,
                colors: Self::ui_color_model(filament_colors),
                material,
            },
            filament_has_alpha,
            spool_has_alpha,
            spool_colors: Self::ui_color_model(spool_colors),
            k: slot.pressure_advance_value.to_shared_string(),
            k_meta: slot.pressure_advance_meta.to_shared_string(),
            tagged: slot.spool_id.is_some(),
            weight_display: self.weight_display_snapshot(slot).to_shared_string(),
            used_in_print: slot.used_in_print,
            spool_id: slot.spool_id.as_deref().unwrap_or_default().to_shared_string(),
        }
    }

    fn non_empty_ui_colors(mut colors: Vec<slint::Color>) -> Vec<slint::Color> {
        if colors.is_empty() {
            colors.push(slint::Color::from_argb_encoded(0xFF000000));
        }
        colors
    }

    fn ui_color_model(colors: Vec<slint::Color>) -> slint::ModelRc<slint::Color> {
        slint::ModelRc::from(Rc::new(slint::VecModel::from(colors)))
    }

    fn ui_slot_group_kind(kind: printer_domain::SlotGroupKind) -> UiSlotGroupKind {
        match kind {
            printer_domain::SlotGroupKind::Mms => UiSlotGroupKind::InternalChanger,
            printer_domain::SlotGroupKind::External => UiSlotGroupKind::External,
            printer_domain::SlotGroupKind::Virtual => UiSlotGroupKind::Virtual,
            printer_domain::SlotGroupKind::Other => UiSlotGroupKind::Other,
        }
    }

    fn ui_slot_state_from_slot_state(state: printer_domain::SlotState) -> UiSlotState {
        match state {
            printer_domain::SlotState::Unknown | printer_domain::SlotState::Error => UiSlotState::Unknown,
            printer_domain::SlotState::Empty => UiSlotState::Empty,
            printer_domain::SlotState::Occupied => UiSlotState::Occupied,
            printer_domain::SlotState::Reading => UiSlotState::Reading,
            printer_domain::SlotState::Ready => UiSlotState::Ready,
            printer_domain::SlotState::Loading => UiSlotState::Loading,
            printer_domain::SlotState::Unloading => UiSlotState::Unloading,
            printer_domain::SlotState::Loaded => UiSlotState::Loaded,
        }
    }

    fn slot_state_label(state: printer_domain::SlotState) -> &'static str {
        match state {
            printer_domain::SlotState::Unknown => "Unknown",
            printer_domain::SlotState::Empty => "Empty",
            printer_domain::SlotState::Occupied => "Occupied",
            printer_domain::SlotState::Reading => "Reading",
            printer_domain::SlotState::Ready => "Ready",
            printer_domain::SlotState::Loading => "Loading",
            printer_domain::SlotState::Unloading => "Unloading",
            printer_domain::SlotState::Loaded => "Loaded",
            printer_domain::SlotState::Error => "Error",
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
                    && code == search_code
                {
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
                if let Some(list_material) = split.next()
                    && list_material == material
                    && let (Some(filament_id), Some(nozzle_temp_low), Some(nozzle_temp_high)) = (split.next(), split.next(), split.next())
                    && let (Ok(_wrong_nozzle_temp_low), Ok(_wrong_nozzle_temp_high)) =
                        (nozzle_temp_low.parse::<u32>(), nozzle_temp_high.parse::<u32>())
                {
                    material_code = filament_id;
                    found = true;
                    break;
                }
            }

            if found {
                for line in BASE_FILAMENTS.lines() {
                    let mut split = line.split(',');
                    if let (Some(code), Some(name), Some(nozzle_temp_low), Some(nozzle_temp_high)) =
                        (split.next(), split.next(), split.next(), split.next())
                        && code == material_code
                    {
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

        None
    }

    fn ui_set_staging_to_slot(&self, slot_id: &str) {
        let ui_borrow = self.ui_weak.unwrap();
        let ui = ui_borrow.global::<crate::app::AppState>();
        let Some((_current_printer, manager_index, snapshot)) = self.selected_printer_snapshot() else {
            error!("No selected printer snapshot for generic slot assignment");
            return;
        };
        let capabilities = self.printer_manager.borrow().capabilities_at(manager_index).unwrap_or_default();
        let full_slot_description = Self::slot_description_from_snapshot(&snapshot, slot_id);

        if !snapshot.connected {
            ui.invoke_slot_operation_failed(
                "Configure".into(),
                full_slot_description.to_shared_string(),
                "Printer disconnected".to_shared_string(),
            );
            return;
        }
        if !capabilities.material_slot_assign {
            ui.invoke_slot_operation_failed(
                "Configure".into(),
                full_slot_description.to_shared_string(),
                "Material assignment unsupported".to_shared_string(),
            );
            return;
        }
        if !capabilities.material_slot_set_spool_id {
            ui.invoke_slot_operation_failed(
                "Configure".into(),
                full_slot_description.to_shared_string(),
                "Spool ID assignment unsupported".to_shared_string(),
            );
            return;
        }

        let full_spool_rec = {
            let filament_staging = self.filament_staging.borrow();
            let Some(full_spool_rec) = filament_staging.full_spool_rec().clone() else {
                return;
            };
            full_spool_rec
        };
        let Some(filament_info) = self.get_filament_info(&full_spool_rec.spool_rec.slicer_filament, Some(&full_spool_rec.spool_rec.material_type))
        else {
            ui.invoke_slot_operation_failed(
                "Configure".into(),
                full_slot_description.to_shared_string(),
                slint::format!("Spool {} Missing Required Information", full_spool_rec.spool_rec.id),
            );
            return;
        };

        let dispatch_result = {
            self.printer_manager.borrow_mut().dispatch_at(
                manager_index,
                PrinterCommand::AssignMaterialToSlot {
                    slot_id: SlotId::new(slot_id),
                    spool: full_spool_rec.clone(),
                    temps: FilamentTemps {
                        nozzle_min_c: Some(filament_info.nozzle_temp_low as u32),
                        nozzle_max_c: Some(filament_info.nozzle_temp_high as u32),
                    },
                    mode: SlotAssignMode::WritePrinterMaterial,
                },
            )
        };

        match dispatch_result {
            Ok(()) => {
                if !full_spool_rec.spool_rec.actual_location.is_empty() {
                    let mut spool_rec = Box::new(full_spool_rec.spool_rec.clone());
                    spool_rec.actual_location = String::new();
                    let _ = self.dispatch_async_task(AppAsyncTaskRequest::UpdateSpoolRec {
                        spool_rec,
                        message_box: None,
                    });
                }
                self.filament_staging.borrow_mut().clear();
                ui.invoke_empty_spool_staging();
                self.update_slot_groups_from_selected_printer();
                ui.invoke_slot_operation_succeeded("Configure".into(), full_slot_description.to_shared_string());
            }
            Err(err) => {
                ui.invoke_slot_operation_failed("Configure".into(), full_slot_description.to_shared_string(), slint::format!("{err:?}"));
            }
        }
    }

    fn ui_reset_slot(&self, slot_id: &str) {
        self.ui_dispatch_slot_command(
            slot_id,
            PrinterCommand::ClearSlot {
                slot_id: SlotId::new(slot_id),
            },
            |capabilities| capabilities.material_slot_clear,
            "Reset",
            "Slot reset unsupported",
        );
    }

    fn ui_unassign_slot(&self, slot_id: &str) {
        self.stage_unassigned_slot_if_possible(slot_id);
        self.ui_dispatch_slot_command(
            slot_id,
            PrinterCommand::UnassignSpoolFromSlot {
                slot_id: SlotId::new(slot_id),
            },
            |capabilities| capabilities.material_slot_unassign_spool,
            "Untag",
            "Slot untag unsupported",
        );
    }

    fn stage_unassigned_slot_if_possible(&self, slot_id: &str) {
        if ![StagingOrigin::Empty, StagingOrigin::Unloaded].contains(self.filament_staging.borrow().origin()) {
            return;
        }

        let Some((_current_printer, _manager_index, snapshot)) = self.selected_printer_snapshot() else {
            return;
        };
        let spool_id = snapshot
            .slot_groups
            .iter()
            .flat_map(|group| group.slots.iter())
            .find(|slot| slot.id.as_str() == slot_id)
            .and_then(|slot| slot.spool_id.as_ref());
        let Some(spool_id) = spool_id else {
            return;
        };
        let Some(spool_rec) = self.store.get_spool_by_id(spool_id) else {
            return;
        };

        self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Unloaded);
        self.display_filament_staging(true);
        let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
    }

    fn ui_dispatch_slot_command(
        &self,
        slot_id: &str,
        command: PrinterCommand,
        supports: impl FnOnce(&printer_domain::PrinterCapabilities) -> bool,
        operation: &str,
        unsupported_message: &str,
    ) {
        let ui_borrow = self.ui_weak.unwrap();
        let ui = ui_borrow.global::<crate::app::AppState>();
        let Some((_current_printer, manager_index, snapshot)) = self.selected_printer_snapshot() else {
            error!("No selected printer snapshot for generic slot operation");
            return;
        };
        let capabilities = self.printer_manager.borrow().capabilities_at(manager_index).unwrap_or_default();
        let full_slot_description = Self::slot_description_from_snapshot(&snapshot, slot_id);

        if !snapshot.connected {
            ui.invoke_slot_operation_failed(
                operation.into(),
                full_slot_description.to_shared_string(),
                "Printer disconnected".to_shared_string(),
            );
            return;
        }
        if !supports(&capabilities) {
            ui.invoke_slot_operation_failed(
                operation.into(),
                full_slot_description.to_shared_string(),
                unsupported_message.to_shared_string(),
            );
            return;
        }

        let dispatch_result = { self.printer_manager.borrow_mut().dispatch_at(manager_index, command) };
        match dispatch_result {
            Ok(()) => {
                self.update_slot_groups_from_selected_printer();
                ui.invoke_slot_operation_succeeded(operation.into(), full_slot_description.to_shared_string());
            }
            Err(err) => {
                ui.invoke_slot_operation_failed(operation.into(), full_slot_description.to_shared_string(), slint::format!("{err:?}"));
            }
        }
    }

    fn ui_load_staging(&self, spool_id: &str) -> SharedString {
        if let Some(spool_rec) = self.store.get_spool_by_id(spool_id) {
            if spool_rec.spools_count <= 1 {
                self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Scanned);
                self.filament_staging.borrow_mut().set_scanned_tag_id(None);
                self.display_filament_staging(true);
                let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
                SharedString::new()
            } else {
                SharedString::from("Can't load Stock")
            }
        } else {
            SharedString::from("Spool Not Found")
        }
    }

    fn ui_storage_rack_count(&self) -> i32 {
        self.store
            .storage_config
            .borrow()
            .rack_config
            .keys()
            .filter(|rack_id| rack_id.parse::<i32>().is_ok())
            .count() as i32
    }

    fn ui_get_storage_rack_options(&self) -> slint::ModelRc<crate::app::SelectorOption> {
        let mut racks = {
            let storage_config = self.store.storage_config.borrow();
            storage_config
                .rack_config
                .iter()
                .filter_map(|(rack_id_str, rack)| rack_id_str.parse::<i32>().ok().map(|rack_id| (rack_id, rack.name.clone())))
                .collect::<Vec<_>>()
        };

        racks.sort_by_key(|(rack_id, _)| *rack_id);

        slint::ModelRc::new(slint::VecModel::from(
            racks
                .into_iter()
                .map(|(rack_id, name)| crate::app::SelectorOption {
                    id: rack_id,
                    text: name.to_shared_string(),
                })
                .collect::<Vec<_>>(),
        ))
    }

    fn ui_storage_rack_value(&self, rack_id: i32, field: StorageRackValue) -> i32 {
        let rack_id_str = rack_id.to_string();
        let storage_config = self.store.storage_config.borrow();
        if let Some(rack) = storage_config.rack_config.get(&rack_id_str) {
            match field {
                StorageRackValue::Bays => rack.num_bays,
                StorageRackValue::Shelves => rack.num_shelves,
                StorageRackValue::Positions => rack.num_positions,
                StorageRackValue::Containers => rack.num_containers,
            }
        } else {
            0
        }
    }

    fn ui_storage_location_str(&self, rack_id: i32, bay: i32, shelf: i32, position: i32, container: i32) -> SharedString {
        if rack_id <= 0 {
            return SharedString::new();
        }

        let mut location = format!("#R:{rack_id}");
        if bay > 0 {
            location.push_str(&format!("/B:{bay}"));
        }
        if shelf > 0 {
            location.push_str(&format!("/S:{shelf}"));
        }
        if position > 0 {
            location.push_str(&format!("/P:{position}"));
        }
        if container > 0 {
            location.push_str(&format!("/C:{container}"));
        }
        location.to_shared_string()
    }

    fn ui_location_str_to_location(&self, location_str: &str) -> crate::app::Location {
        let mut location = crate::app::Location::default();

        if let Some(input) = location_str.strip_prefix('#') {
            for segment in input.split('/') {
                if let Some((key, value_str)) = segment.split_once(':')
                    && let Ok(value) = value_str.parse::<i32>()
                {
                    match key {
                        "R" => {
                            location.rack = value;
                            if let Some(rack) = self.store.storage_config.borrow().rack_config.get(value_str) {
                                location.rack_name = rack.name.to_shared_string();
                            }
                        }
                        "B" => location.bay = value,
                        "S" => location.shelf = value,
                        "P" => location.position = value,
                        "C" => location.container = value,
                        _ => (),
                    }
                }
            }
        }
        location
    }

    fn ui_update_actual_location(&self, spool_id: &str, location: &str, message: &str) {
        let store = self.store.clone();
        if let Some(mut spool_rec) = store.get_spool_by_id(spool_id).map(Box::new) {
            spool_rec.actual_location = location.to_string();
            let message_box = Some(MessageBox {
                title: "Storage Notice".to_string(),
                text: message.to_string(),
                text2: "".to_string(),
                timeout: -1,
            });
            let _ = self.dispatch_async_task(AppAsyncTaskRequest::UpdateSpoolRec { spool_rec, message_box });
        } else {
            self.message_box(
                "Unexpected Internal Error",
                &format!("Spool {}  not Found", spool_id),
                "",
                crate::app::StatusType::Error,
                0,
            );
        }
    }

    fn ui_encode_location_tag(&self) {
        const LOCATION_URL_PREFIX_V1: &str = "https://tag.spoolease.io/L1/";
        let spool_tag_borrow = self.spool_tag_model.borrow();
        let descriptor = format!("{LOCATION_URL_PREFIX_V1}?TG={TAG_PLACEHOLDER}");
        let encode_cookie = LocationEncodeCookie { location: String::new() };
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

    fn ui_configure_slot_with_spool_id(&self, slot_id: &str, spool_id: &str) {
        let Some((_current_printer, _manager_index, snapshot)) = self.selected_printer_snapshot() else {
            error!("No selected printer snapshot for slot reconfigure");
            return;
        };
        let _ = self.dispatch_async_task(AppAsyncTaskRequest::ConfigureSlotWithSpool {
            printer_id: snapshot.id,
            slot_id: SlotId::new(slot_id),
            spool_id: spool_id.to_string(),
            only_spool_id: false,
        });
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

    fn ui_erase_staging_tag(&self) -> bool {
        let filament_staging_borrow = self.filament_staging.borrow();
        if let Some(spool_rec) = filament_staging_borrow.spool_rec() {
            if !spool_rec.has_valid_tag_id() {
                // no tags for this spool
                error!("Received to erase staging tag when spool is not tagged");
                let ui = self.ui_weak.unwrap();
                let ui_app_state: crate::app::AppState<'_> = ui.global::<crate::app::AppState>();
                ui_app_state.invoke_show_message_box(
                    "Erase Tag Notice".into(),
                    slint::format!("Spool {} is not linked to a Tag", spool_rec.id),
                    SharedString::new(),
                    crate::app::StatusType::Error,
                    -1,
                );
                false
            } else if spool_rec.tag_id.len() == 1 {
                self.ui_erase_tag(&spool_rec.tag_id[0]);
                true
            } else if let Some(scanned_tag_id) = filament_staging_borrow.scanned_tag_id() {
                self.ui_erase_tag(scanned_tag_id);
                true
            } else {
                // error case
                error!("Received to erase staging tag when which tag is not clear");
                let ui = self.ui_weak.unwrap();
                let ui_app_state: crate::app::AppState<'_> = ui.global::<crate::app::AppState>();
                ui_app_state.invoke_show_message_box(
                    "Erase Tag Notice".into(),
                    slint::format!("Can't tell which of Spool {} tags to erase", spool_rec.id),
                    SharedString::new(),
                    crate::app::StatusType::Error,
                    -1,
                );
                false
            }
        } else {
            // noh spool in staging ??
            error!("Unexpected error: Arrived to erase staging tag when no spool in staging");
            false
        }
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

    fn ui_import_definition_tag_to_inventory(
        &self,
        tag_definition_type: &str,
        tag_definition_info: &str,
        empty_spool_weight: i32,
        spool_is_full: bool,
    ) {
        let _ = self.dispatch_async_task(AppAsyncTaskRequest::ImportDefinitionTagToInventory {
            tag_definition_type: tag_definition_type.to_string(),
            tag_definition_info: tag_definition_info.to_string(),
            empty_spool_weight,
            spool_is_full,
        });
    }

    fn ui_can_link_untagged_spool_to_tag(&self, id: &str) -> SharedString {
        if let Some(spool_rec) = self.store.get_spool_by_id(id) {
            if spool_rec.spools_count <= 1 {
                if !spool_rec.has_valid_tag_id() {
                    SharedString::new()
                } else {
                    SharedString::from("Spool Is Tagged")
                }
            } else {
                SharedString::from("Can't link Stock")
            }
        } else {
            SharedString::from("Spool Not Found")
        }
    }

    fn ui_can_link_tagged_spool_to_tag(&self, id: &str) -> SharedString {
        if let Some(spool_rec) = self.store.get_spool_by_id(id) {
            if spool_rec.spools_count <= 1 {
                if spool_rec.has_valid_tag_id() {
                    SharedString::new()
                } else {
                    SharedString::from("Spool Is Not Tagged")
                }
            } else {
                SharedString::from("Can't link Stock")
            }
        } else {
            SharedString::from("Spool Not Found")
        }
    }

    fn ui_spool_tag_count(&self, spool_id: &str) -> i32 {
        self.store
            .get_spool_by_id(spool_id)
            .map(|spool_rec| spool_rec.linked_tag_ids().count() as i32)
            .unwrap_or_default()
    }

    fn ui_staging_scanned_tag_id(&self) -> SharedString {
        self.filament_staging.borrow().scanned_tag_id().unwrap_or_default().to_shared_string()
    }

    fn ui_get_slot_display(&self, slot_id: &str) -> UiSlotDisplay {
        let Some((_current_printer, _manager_index, snapshot)) = self.selected_printer_snapshot() else {
            return UiSlotDisplay::default();
        };
        let Some(slot) = snapshot
            .slot_groups
            .iter()
            .flat_map(|group| group.slots.iter())
            .find(|slot| slot.id.as_str() == slot_id)
        else {
            return UiSlotDisplay::default();
        };

        self.ui_get_slot_display_from_snapshot(slot)
    }

    fn ui_get_slot_display_from_snapshot(&self, slot: &printer_domain::MaterialSlotSnapshot) -> UiSlotDisplay {
        let (filament_title, slicer_name, color_code, temp_min, temp_max) = match &slot.filament {
            printer_domain::PrinterFilament::Known(filament) => self.ui_slot_filament_display(filament),
            printer_domain::PrinterFilament::Unknown => Default::default(),
        };

        UiSlotDisplay {
            available_in_spool: self.weight_left_snapshot(slot).unwrap_or_default(),
            color_code,
            consumed_since_loaded: slot.consumed_since_load_g,
            filament_title,
            slicer_name,
            temp_max,
            temp_min,
            pa: slot.pressure_advance_value.to_shared_string(),
            pa_meta: slot.pressure_advance_meta.to_shared_string(),
        }
    }

    fn ui_slot_filament_display(&self, filament: &printer_domain::PrinterFilamentInfo) -> (SharedString, SharedString, SharedString, i32, i32) {
        let material = if filament.material_type.is_empty() {
            filament.slicer_filament.as_str()
        } else {
            filament.material_type.as_str()
        };
        let (slicer_name, temp_min, temp_max) = if let Some(filament_info) = self.get_filament_info(&filament.slicer_filament, Some(material)) {
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
            (
                filament.slicer_filament.to_shared_string(),
                filament.temps.nozzle_min_c.unwrap_or_default() as i32,
                filament.temps.nozzle_max_c.unwrap_or_default() as i32,
            )
        };
        let brand = if !slicer_name.is_empty() {
            if filament.brand.is_empty() {
                get_brand_from_text(slicer_name.as_str()).unwrap_or("")
            } else {
                filament.brand.as_str()
            }
        } else {
            ""
        };
        let color_name = if !filament.color_name.is_empty() {
            format!(" {}", filament.color_name)
        } else if brand == "Bambu" {
            Self::bambu_filament_color_name(&filament.slicer_filament, &filament.color_codes)
        } else {
            String::new()
        };
        let filament_title = format!("{brand} {material}{color_name}").trim().to_shared_string();
        (
            filament_title,
            slicer_name,
            filament.color_codes.join(";").to_shared_string(),
            temp_min,
            temp_max,
        )
    }

    fn bambu_filament_color_name(slicer_filament: &str, color_codes: &[String]) -> String {
        let mut colors_rgba_for_compare = color_codes.iter().map(|s| s.as_str()).collect::<Vec<_>>();
        colors_rgba_for_compare.sort();
        BAMBU_COLOR_NAMES
            .lines()
            .find_map(|line| {
                let mut s = line.split(',');
                let id = s.next()?;
                if id != slicer_filament {
                    return None;
                }

                let colors = s.next()?;
                let mut color_name_colors = colors.split('/').collect::<Vec<_>>();
                color_name_colors.sort();

                if colors_rgba_for_compare != color_name_colors {
                    return None;
                }

                let name = s.next()?;
                let code = s.next()?;
                Some(format!(" {name} ({code})"))
            })
            .unwrap_or_default()
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

        let color_code = spool_rec
            .color_code
            .iter()
            .map(String::as_str)
            .filter(|color| !color.is_empty())
            .collect::<Vec<_>>()
            .join(";");
        let record = UiSpoolRecord {
            brand: spool_rec.brand.into(),
            color_code: color_code.into(),
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

        let parsed_colors = Self::ui_colors_from_color_codes(&spool_rec.color_code);
        let color = parsed_colors.first().copied().unwrap_or_default();
        let colors_has_alpha = parsed_colors.iter().any(|color| color.alpha() < 255);
        let colors = slint::ModelRc::from(Rc::new(slint::VecModel::from(parsed_colors)));

        UiSpoolRecordDisplay {
            slicer_filament_name,
            spool_record: record,
            temp_min,
            temp_max,
            color,
            colors,
            colors_has_alpha,
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

        let color_code = spool_rec
            .color_code
            .iter()
            .map(String::as_str)
            .filter(|color| !color.is_empty())
            .collect::<Vec<_>>()
            .join(";");
        let record = UiSpoolRecord {
            added_full: spool_rec.added_full.unwrap_or_default(),
            // added_time: todo!(),
            brand: spool_rec.brand.into(),
            color_code: color_code.into(),
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
            weight_advertised: spool_rec.weight_advertised.unwrap_or_default(),
            weight_core: spool_rec.weight_core.unwrap_or_default(),
            weight_current: spool_rec.weight_current.unwrap_or_default(),
            weight_new: spool_rec.weight_new.unwrap_or_default(),
            actual_location: spool_rec.actual_location.into(),
            assigned_location: spool_rec.assigned_location.to_shared_string(),
        };

        // Changed this (for now, on purpose, not filling in fields that aren't in the tag, to show the real tag information)
        let (slicer_filament_name, temp_min, temp_max) =
            if let Some(filament_info) = &self.get_filament_info(&record.slicer_filament, Some(&record.material_type)) {
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

        let parsed_colors = Self::ui_colors_from_color_codes(&spool_rec.color_code);
        let color = parsed_colors.first().copied().unwrap_or_default();
        let colors_has_alpha = parsed_colors.iter().any(|color| color.alpha() < 255);
        let colors = slint::ModelRc::from(Rc::new(slint::VecModel::from(parsed_colors)));

        let assigned_location;
        if let Some(location_str) = spool_rec.assigned_location.strip_prefix("#R:") {
            if let Some((rack_id_str, rest)) = location_str.split_once('/') {
                let storage_config = self.store.storage_config.borrow();
                if let Some(rack) = storage_config.rack_config.get(rack_id_str) {
                    assigned_location = slint::format!("{}/{rest}", rack.name);
                } else {
                    assigned_location = spool_rec.assigned_location.into();
                }
            } else {
                assigned_location = spool_rec.assigned_location.into();
            }
        } else if let Some(location_str) = spool_rec.assigned_location.strip_prefix("@") {
            assigned_location = location_str.to_shared_string();
        } else {
            assigned_location = spool_rec.assigned_location.into();
        };

        UiSpoolRecordDisplay {
            pa_line1: (if record.ext_has_k { "Configured" } else { "Not Configured" }).to_shared_string(),
            pa_line2: SharedString::new(),
            slicer_filament_name,
            spool_record: record,
            temp_min,
            temp_max,
            color,
            colors,
            colors_has_alpha,
            weight_left,
            assigned_location,
        }
    }

    fn tag_info_to_ui_spool_info_direct(&self, full_spool_rec: &Option<FullSpoolRecord>) -> Option<crate::app::UiSpoolRecordDisplay> {
        full_spool_rec
            .as_ref()
            .map(|full_spool_rec| self.ui_get_spool_record_display(&full_spool_rec.spool_rec.id.to_shared_string()))
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

    fn display_filament_staging_direct(&self, finish_operation: bool) {
        let filament_staging_borrow = self.filament_staging.borrow();
        if let Some(ui_spool_info) = self.tag_info_to_ui_spool_info_direct(filament_staging_borrow.full_spool_rec()) {
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
        self.display_filament_staging_direct(notify_operation);
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

    async fn import_definition_tag_to_inventory_async(
        view_model: Rc<RefCell<ViewModel>>,
        tag_definition_type: String,
        tag_definition_info: String,
        empty_spool_weight: i32,
        spool_is_full: bool,
    ) {
        let (spool_rec, origin_data) = match tag_definition_type.as_str() {
            BAMBULAB_TAG_TYPE => {
                if let Ok(bambulab_tag) = serde_json::from_str::<BambuLabTag>(&tag_definition_info) {
                    let mut spool_rec = bambulab_tag.to_spool_rec();
                    spool_rec.added_full = Some(spool_is_full);
                    (Some(spool_rec), Some(OriginData::BambuLabTag { bambulab_tag }))
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

    async fn link_tag_to_spool_id_async(
        view_model: Rc<RefCell<ViewModel>>,
        tag_id: String,
        tag_type: String,
        spool_id: String,
        mode: LinkTagMode,
        final_step: bool,
    ) {
        let store = view_model.borrow().store.clone();
        if let Some(mut spool_rec) = store.get_spool_by_id(&spool_id) {
            let ui = view_model.borrow().ui_weak.unwrap();
            let ui_app_state = ui.global::<crate::app::AppState>();

            if spool_rec.spools_count > 1 {
                ui_app_state.invoke_link_tag_to_spool_id_status("Can't link Stock".into());
                return;
            }

            match mode {
                LinkTagMode::ToUntaggedSpool => {
                    if spool_rec.has_valid_tag_id() {
                        ui_app_state.invoke_link_tag_to_spool_id_status("Spool Is Tagged".into());
                        return;
                    }
                }
                LinkTagMode::ToTaggedSpool => {
                    if !spool_rec.has_valid_tag_id() {
                        ui_app_state.invoke_link_tag_to_spool_id_status("Spool Is Not Tagged".into());
                        return;
                    }
                    if spool_rec.linked_tag_ids().any(|existing_tag_id| existing_tag_id == tag_id) {
                        ui_app_state.invoke_link_tag_to_spool_id_status("Tag Already Linked to Spool".into());
                        return;
                    }
                }
            }

            if let Some(existing_spool) = store.get_spool_by_hex_tag(&tag_id)
                && existing_spool.id != spool_id
            {
                ui_app_state.invoke_link_tag_to_spool_id_status(format!("Tag Already Linked to Spool {}", existing_spool.id).to_shared_string());
                return;
            }

            spool_rec.tag_id = match mode {
                LinkTagMode::ToUntaggedSpool => vec![tag_id.clone()],
                LinkTagMode::ToTaggedSpool => {
                    let mut tag_ids = spool_rec.tag_id;
                    tag_ids.push(tag_id.clone());
                    tag_ids
                }
            };
            spool_rec.tag_type = tag_type.clone();
            let store_res = store.update_spool(spool_rec.clone(), None).await;
            match store_res {
                Ok(_) => {
                    ui_app_state.invoke_link_tag_to_spool_id_status(SharedString::new());
                    view_model
                        .borrow()
                        .filament_staging
                        .borrow_mut()
                        .set_spool_record(spool_rec, StagingOrigin::Scanned);
                    view_model.borrow().filament_staging.borrow_mut().set_scanned_tag_id(Some(tag_id.clone()));
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

    async fn unlink_spool_tags_async(view_model: Rc<RefCell<ViewModel>>, spool_id: String, mode: UnlinkTagMode) {
        let (store, scanned_tag_id, ui) = {
            let view_model_borrow = view_model.borrow();
            (
                view_model_borrow.store.clone(),
                view_model_borrow.filament_staging.borrow().scanned_tag_id().map(ToString::to_string),
                view_model_borrow.ui_weak.unwrap(),
            )
        };
        let ui_app_state = ui.global::<crate::app::AppState>();

        if let Some(mut spool_rec) = store.get_spool_by_id(&spool_id) {
            let unlink_message = match mode {
                UnlinkTagMode::AllTags => {
                    spool_rec.tag_id.clear();
                    spool_rec.tag_type = "".to_string();
                    spool_rec.encode_time = None;
                    format!("Spool {spool_id} Unlinked From All Tags")
                }
                UnlinkTagMode::ScannedTag => {
                    let Some(scanned_tag_id) = scanned_tag_id else {
                        ui_app_state.invoke_unlink_spool_id_tags_status(
                            spool_id.to_shared_string(),
                            false,
                            "Unlink operation is no longer valid".into(),
                        );
                        return;
                    };
                    if spool_rec.linked_tag_ids().count() <= 1 {
                        ui_app_state.invoke_unlink_spool_id_tags_status(
                            spool_id.to_shared_string(),
                            false,
                            "No choice available: spool has one tag".into(),
                        );
                        return;
                    }
                    let prev_len = spool_rec.tag_id.len();
                    spool_rec.tag_id.retain(|tag_id| tag_id != &scanned_tag_id);
                    if spool_rec.tag_id.len() == prev_len {
                        ui_app_state.invoke_unlink_spool_id_tags_status(
                            spool_id.to_shared_string(),
                            false,
                            "Scanned tag is not linked to this spool".into(),
                        );
                        return;
                    }
                    format!("Spool {spool_id} Unlinked From Scanned Tag")
                }
            };

            match store.update_spool(spool_rec.clone(), None).await {
                Ok(_) => {
                    let filament_staging_rc = {
                        let view_model_borrow = view_model.borrow();
                        view_model_borrow.filament_staging.clone()
                    };
                    let mut filament_staging = filament_staging_rc.borrow_mut();
                    match mode {
                        UnlinkTagMode::AllTags => {
                            filament_staging.clear();
                            drop(filament_staging);
                            ui_app_state.invoke_empty_spool_staging();
                        }
                        UnlinkTagMode::ScannedTag => {
                            filament_staging.update_spool_rec_keep_rest(spool_rec);
                            filament_staging.set_scanned_tag_id(None);
                            drop(filament_staging);
                            view_model.borrow().display_filament_staging(false);
                        }
                    }

                    ui_app_state.invoke_unlink_spool_id_tags_status(spool_id.to_shared_string(), true, unlink_message.into());
                }
                Err(err) => {
                    error!("Failed to unlink tags for spool_id {spool_id} ({mode:?}): {err:?}");
                    ui_app_state.invoke_unlink_spool_id_tags_status(
                        spool_id.to_shared_string(),
                        false,
                        format!("Failed to unlink tags for spool {spool_id}: {err}").into(),
                    );
                }
            }
        } else {
            error!("Failed to unlink tags for spool_id {spool_id} ({mode:?}): Spool Id not found");
            ui_app_state.invoke_unlink_spool_id_tags_status(spool_id.to_shared_string(), false, format!("Spool Id {spool_id} Not Found").into());
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
            if weight_new >= 0 {
                // ignore -1 (and potentially other negative numbers which are invalid)
                spool_rec.weight_new = Some(weight_new);
                if weight_new != 0 {
                    spool_rec.added_full = Some(true);
                    // in this case also potentially update empty if not already filled earlier
                    if spool_rec.weight_core.unwrap_or(0) == 0 && spool_rec.weight_advertised.unwrap_or(0) != 0 {
                        let new_weight_core = weight_new - spool_rec.weight_advertised.unwrap();
                        if new_weight_core > 0 {
                            spool_rec.weight_core = Some(new_weight_core);
                        }
                    }
                } // else - don't touch added-full or weight_core
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

    async fn update_spool_rec_async(view_model: Rc<RefCell<ViewModel>>, spool_rec: Box<SpoolRecord>, message_box: Option<MessageBox>) {
        let store = view_model.borrow().store.clone();
        match store.update_spool(*spool_rec.clone(), None).await {
            Ok(_) => {
                // Check if need to update staging, and do if needed
                let view_model_borrow = view_model.borrow();
                let need_replace_staging = if let Some(staging_spool_rec) = view_model_borrow.filament_staging.borrow().spool_rec() {
                    staging_spool_rec.id == spool_rec.id
                } else {
                    false
                };
                if need_replace_staging {
                    {
                        view_model_borrow.filament_staging.borrow_mut().update_spool_rec_keep_rest(*spool_rec);
                    }
                    view_model_borrow.display_filament_staging(false);
                }
                if let Some(message_box) = message_box {
                    view_model_borrow.message_box(
                        &message_box.title,
                        &message_box.text,
                        &message_box.text2,
                        crate::app::StatusType::Success,
                        message_box.timeout,
                    );
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

    async fn configure_slot_with_spool_async(
        view_model: Rc<RefCell<ViewModel>>,
        printer_id: PrinterId,
        slot_id: SlotId,
        spool_id: String,
        only_spool_id: bool,
    ) {
        let (manager_index, snapshot) = match view_model.borrow().printer_snapshot_by_id(&printer_id) {
            Some(snapshot) => snapshot,
            None => {
                error!(
                    "Printer {} not found when trying to configure slot {}",
                    printer_id.as_str(),
                    slot_id.as_str()
                );
                view_model.borrow().message_box(
                    "Configure Slot Notice",
                    &format!("Printer {} Not Found", printer_id.as_str()),
                    "",
                    crate::app::StatusType::Error,
                    0,
                );
                return;
            }
        };
        let capabilities = view_model
            .borrow()
            .printer_manager
            .borrow()
            .capabilities_at(manager_index)
            .unwrap_or_default();
        let full_slot_description = Self::slot_description_from_snapshot(&snapshot, slot_id.as_str());
        let ui = view_model.borrow().ui_weak.unwrap();
        let ui_app_state = ui.global::<crate::app::AppState>();

        if !snapshot.connected {
            ui_app_state.invoke_slot_operation_failed("Configure".into(), full_slot_description.into(), "Printer disconnected".into());
            return;
        }
        if !(capabilities.material_slot_assign && capabilities.material_slot_set_spool_id) {
            ui_app_state.invoke_slot_operation_failed("Configure".into(), full_slot_description.into(), "Material assignment unsupported".into());
            return;
        }

        let store = view_model.borrow().store.clone();
        if let Some(spool_rec) = store.get_spool_by_id(&spool_id) {
            let mut full_spool_rec = FullSpoolRecord {
                spool_rec,
                spool_rec_ext: SpoolRecordExt::default(),
            };
            if !only_spool_id && full_spool_rec.spool_rec.ext_has_k {
                match store.get_spool_ext_by_id(&spool_id).await {
                    Ok(spool_rec_ext) => {
                        full_spool_rec.spool_rec_ext = spool_rec_ext;
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
                let mode = if only_spool_id {
                    SlotAssignMode::SpoolIdOnly
                } else {
                    SlotAssignMode::WritePrinterMaterial
                };
                let dispatch_result = view_model.borrow().printer_manager.borrow_mut().dispatch_by_id(
                    &printer_id,
                    PrinterCommand::AssignMaterialToSlot {
                        slot_id: slot_id.clone(),
                        spool: full_spool_rec.clone(),
                        temps: FilamentTemps {
                            nozzle_min_c: Some(filament_info.nozzle_temp_low as u32),
                            nozzle_max_c: Some(filament_info.nozzle_temp_high as u32),
                        },
                        mode,
                    },
                );
                match dispatch_result {
                    Ok(()) => {
                        if !full_spool_rec.spool_rec.actual_location.is_empty() {
                            let mut spool_rec = Box::new(full_spool_rec.spool_rec.clone());
                            spool_rec.actual_location = String::new();
                            let _ = view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::UpdateSpoolRec {
                                spool_rec,
                                message_box: None,
                            });
                        }
                        let selected_manager_index = {
                            let view_model_borrow = view_model.borrow();
                            view_model_borrow.selected_manager_index()
                        };
                        if selected_manager_index == Some(manager_index) {
                            view_model.borrow().update_slot_groups_from_selected_printer();
                        }
                        let target_printer_id = view_model.borrow().printer_id_for_manager_index(manager_index).unwrap_or_default();
                        ui_app_state.invoke_slot_update_succeeded(
                            "Configure".into(),
                            full_slot_description.into(),
                            slot_id.as_str().into(),
                            target_printer_id.into(),
                        );
                    }
                    Err(err) => {
                        ui_app_state.invoke_slot_operation_failed("Configure".into(), full_slot_description.into(), slint::format!("{err:?}"))
                    }
                }
            } else {
                error!("Failed to resolve filament temps for spool {spool_id}");
                ui_app_state.invoke_slot_operation_failed(
                    "Configure".into(),
                    full_slot_description.into(),
                    slint::format!("Spool {spool_id} Missing Required Information"),
                );
            }
        } else {
            error!("Spool {spool_id} not found when trying to configure slot {}", slot_id.as_str());
            ui_app_state.invoke_slot_operation_failed(
                "Configure".into(),
                full_slot_description.into(),
                format!("Spool {spool_id} Not Found").into(),
            );
        }
    }

    async fn configure_staging_to_slot_async(view_model: Rc<RefCell<ViewModel>>, printer_id: PrinterId, slot_id: SlotId) {
        let (manager_index, snapshot) = match view_model.borrow().printer_snapshot_by_id(&printer_id) {
            Some(snapshot) => snapshot,
            None => {
                error!(
                    "Printer {} not found when trying to configure slot {} from staging",
                    printer_id.as_str(),
                    slot_id.as_str()
                );
                return;
            }
        };
        let capabilities = view_model
            .borrow()
            .printer_manager
            .borrow()
            .capabilities_at(manager_index)
            .unwrap_or_default();
        let full_slot_description = Self::slot_description_from_snapshot(&snapshot, slot_id.as_str());
        let ui = view_model.borrow().ui_weak.unwrap();
        let ui_app_state = ui.global::<crate::app::AppState>();

        if !snapshot.connected {
            ui_app_state.invoke_slot_operation_failed("Configure".into(), full_slot_description.into(), "Printer disconnected".into());
            return;
        }
        if !(capabilities.material_slot_assign && capabilities.material_slot_set_spool_id) {
            ui_app_state.invoke_slot_operation_failed("Configure".into(), full_slot_description.into(), "Material assignment unsupported".into());
            return;
        }

        let full_spool_rec = {
            let view_model_borrow = view_model.borrow();
            let filament_staging = view_model_borrow.filament_staging.borrow();
            let Some(full_spool_rec) = filament_staging.full_spool_rec().clone() else {
                return;
            };
            full_spool_rec
        };
        let filament_info = view_model
            .borrow()
            .get_filament_info(&full_spool_rec.spool_rec.slicer_filament, Some(&full_spool_rec.spool_rec.material_type));
        let Some(filament_info) = filament_info else {
            ui_app_state.invoke_slot_operation_failed(
                "Configure".into(),
                full_slot_description.into(),
                slint::format!("Spool {} Missing Required Information", full_spool_rec.spool_rec.id),
            );
            return;
        };

        let dispatch_result = view_model.borrow().printer_manager.borrow_mut().dispatch_by_id(
            &printer_id,
            PrinterCommand::AssignMaterialToSlot {
                slot_id: slot_id.clone(),
                spool: full_spool_rec.clone(),
                temps: FilamentTemps {
                    nozzle_min_c: Some(filament_info.nozzle_temp_low as u32),
                    nozzle_max_c: Some(filament_info.nozzle_temp_high as u32),
                },
                mode: SlotAssignMode::WritePrinterMaterial,
            },
        );

        match dispatch_result {
            Ok(()) => {
                if !full_spool_rec.spool_rec.actual_location.is_empty() {
                    let mut spool_rec = Box::new(full_spool_rec.spool_rec.clone());
                    spool_rec.actual_location = String::new();
                    let _ = view_model.borrow().dispatch_async_task(AppAsyncTaskRequest::UpdateSpoolRec {
                        spool_rec,
                        message_box: None,
                    });
                }
                view_model.borrow().filament_staging.borrow_mut().clear();
                ui_app_state.invoke_empty_spool_staging();

                let selected_manager_index = {
                    let view_model_borrow = view_model.borrow();
                    view_model_borrow.selected_manager_index()
                };
                if selected_manager_index == Some(manager_index) {
                    view_model.borrow().update_slot_groups_from_selected_printer();
                }
                let target_printer_id = view_model.borrow().printer_id_for_manager_index(manager_index).unwrap_or_default();
                ui_app_state.invoke_slot_update_succeeded(
                    "Configure".into(),
                    full_slot_description.into(),
                    slot_id.as_str().into(),
                    target_printer_id.into(),
                );
            }
            Err(err) => {
                ui_app_state.invoke_slot_operation_failed("Configure".into(), full_slot_description.into(), slint::format!("{err:?}"));
            }
        }
    }

    async fn handle_material_slot_presence_changed_async(
        view_model: Rc<RefCell<ViewModel>>,
        printer_id: PrinterId,
        changes: Vec<MaterialSlotPresenceChange>,
    ) {
        let inserted_changes: Vec<MaterialSlotPresenceChange> = changes
            .iter()
            .filter(|change| change.change == MaterialSlotPresenceChangeKind::Inserted)
            .cloned()
            .collect();
        if inserted_changes.len() == 1 {
            let inserted_change = &inserted_changes[0];
            info!("Single slot {} is loading now", inserted_change.slot_id.as_str());
            let staging_has_loaded_spool = {
                let view_model_borrow = view_model.borrow();
                ![StagingOrigin::Unloaded, StagingOrigin::Empty].contains(view_model_borrow.filament_staging.borrow().origin())
            };
            if staging_has_loaded_spool {
                view_model
                    .borrow()
                    .ui_weak
                    .unwrap()
                    .global::<crate::app::AppState>()
                    .invoke_spool_loaded_when_staging_loaded();
                Self::configure_staging_to_slot_async(view_model.clone(), printer_id.clone(), inserted_change.slot_id.clone()).await;
            } else if let Some(spool_id) = &inserted_change.spool_id {
                Self::configure_slot_with_spool_async(
                    view_model.clone(),
                    printer_id.clone(),
                    inserted_change.slot_id.clone(),
                    spool_id.clone(),
                    false,
                )
                .await;
            }
        }

        let removed_spool_id = changes
            .iter()
            .find(|change| change.change == MaterialSlotPresenceChangeKind::Removed)
            .and_then(|change| change.spool_id.clone());
        if let Some(spool_id) = removed_spool_id {
            let can_stage_removed_spool = {
                let view_model_borrow = view_model.borrow();
                [StagingOrigin::Empty, StagingOrigin::Unloaded].contains(view_model_borrow.filament_staging.borrow().origin())
            };
            let spool_rec = if can_stage_removed_spool {
                view_model.borrow().store.get_spool_by_id(&spool_id)
            } else {
                None
            };
            if let Some(spool_rec) = spool_rec {
                view_model
                    .borrow()
                    .filament_staging
                    .borrow_mut()
                    .set_spool_record(spool_rec, StagingOrigin::Unloaded);
                view_model.borrow().display_filament_staging(true);
                Self::set_staging_rec_ext_async(view_model.clone()).await;
            }
        }
    }

    pub fn get_spools_in_printers(&self) -> HashMap<String, String> {
        // spool_id,
        // location: A1 / B3 /... or Ext
        let mut locations = HashMap::new();
        let printer_manager = self.printer_manager.borrow();
        let num_of_printers = printer_manager.len();
        for printer_index in 0..num_of_printers {
            let Some(snapshot) = printer_manager.snapshot_at(printer_index) else {
                continue;
            };
            for slot in snapshot.slot_groups.iter().flat_map(|group| group.slots.iter()) {
                let Some(spool_id) = &slot.spool_id else {
                    continue;
                };
                let slot_name = if slot.short_name.is_empty() {
                    slot.display_name.as_str()
                } else {
                    slot.short_name.as_str()
                };
                if num_of_printers > 1 {
                    locations.insert(spool_id.clone(), format!("{} @ {}", slot_name, snapshot.name));
                } else {
                    locations.insert(spool_id.clone(), format!("{} @ Printer", slot_name));
                }
            }
        }
        locations
    }

    pub fn get_api_printer_slots(&self) -> ApiPrinterSlotsResponse {
        let printer_manager = self.printer_manager.borrow();
        let printers = (0..printer_manager.len())
            .filter_map(|printer_index| {
                let snapshot = printer_manager.snapshot_at(printer_index)?;
                let slot_groups = snapshot
                    .slot_groups
                    .iter()
                    .map(|group| ApiPrinterSlotGroup {
                        id: group.id.clone(),
                        native_id: group.native_id.clone(),
                        kind: group.kind,
                        slots: group
                            .slots
                            .iter()
                            .map(|slot| ApiPrinterSlot {
                                id: slot.id.as_str().to_string(),
                                native_id: slot.native_id.clone(),
                                spool_id: slot.spool_id.clone(),
                                weight_net: self.weight_left_snapshot(slot).filter(|weight| weight.is_finite()),
                            })
                            .collect(),
                    })
                    .collect();

                Some(ApiPrinterSlotsPrinter {
                    id: snapshot.id.0,
                    native_id: snapshot.native_id,
                    kind: snapshot.kind,
                    slot_groups,
                })
            })
            .collect();

        ApiPrinterSlotsResponse { printers }
    }

    pub fn get_printers_filament_pa(&self, filament_id: &str) -> Vec<(String, String, u32, Vec<BambuPressureAdvanceEntry>)> {
        let printer_manager = self.printer_manager.borrow();
        let mut printers = Vec::new();
        for printer_index in 0..printer_manager.len() {
            let Some(snapshot) = printer_manager.snapshot_at(printer_index) else {
                continue;
            };
            if snapshot.kind != printer_domain::PrinterDriverKind::Bambu {
                continue;
            }
            let entries = match printer_manager.query_driver_specific_at(
                printer_index,
                DriverSpecificQuery::Bambu(BambuDriverQuery::PressureAdvanceEntries {
                    filament_id: filament_id.to_string(),
                }),
            ) {
                Ok(DriverSpecificQueryResult::Bambu(BambuDriverQueryResult::PressureAdvanceEntries(entries))) => entries,
                Err(err) => {
                    error!("Failed to query Bambu pressure advance entries for {}: {err:?}", snapshot.native_id);
                    Vec::new()
                }
            };
            printers.push((snapshot.native_id, snapshot.name, snapshot.num_extruders, entries));
        }
        printers
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
        let command = PrinterCommand::DriverSpecific(DriverSpecificCommand::Bambu(BambuDriverCommand::AddPressureAdvance(
            BambuAddPressureAdvance {
                extruder: extruder_id,
                diameter: nozzle_diameter.to_string(),
                nozzle_id: nozzle_id.to_string(),
                filament_id: filament_id.to_string(),
                setting_id: setting_id.to_string(),
                k_value: k_value.to_string(),
                name: name.to_string(),
            },
        )));
        let printer_id = {
            let printer_manager = self.printer_manager.borrow();
            (0..printer_manager.len()).find_map(|printer_index| {
                let snapshot = printer_manager.snapshot_at(printer_index)?;
                (snapshot.kind == printer_domain::PrinterDriverKind::Bambu && snapshot.native_id == printer_serial).then_some(snapshot.id)
            })
        }
        .ok_or_else(|| "Printer not found".to_string())?;

        self.printer_manager
            .borrow_mut()
            .dispatch_by_id(&printer_id, command)
            .map_err(|err| format!("{err:?}"))
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
    pub fn get_printers_status(&self) -> Vec<PrinterInfo> {
        let mut printers_info = Vec::new();
        let snapshots = {
            let printer_manager = self.printer_manager.borrow();
            (0..printer_manager.len())
                .filter_map(|printer_index| {
                    let snapshot = printer_manager.snapshot_at(printer_index);
                    if snapshot.is_none() {
                        error!("Missing printer snapshot for printer index {printer_index}");
                    }
                    snapshot
                })
                .collect::<Vec<_>>()
        };

        for snapshot in snapshots {
            let native_id = snapshot.native_id.clone();

            let slots_sets = self.slot_sets_from_snapshot(&snapshot);
            let slot_set_display_groups = Self::slot_set_display_groups_from_snapshot(&snapshot);
            let internal_changer_count = snapshot
                .slot_groups
                .iter()
                .filter(|group| group.kind == printer_domain::SlotGroupKind::Mms)
                .count() as u32;

            let printer_info = PrinterInfo {
                printer_name: snapshot.name.clone(),
                printer_serial: native_id,
                connected: snapshot.connected,
                num_ams: Some(internal_changer_count),
                print_state: Self::print_state_from_snapshot(snapshot.print.state),
                progress_percent: snapshot.print.progress_percent.map(i32::from),
                remain_secs: snapshot.print.remaining_minutes.map(|v| (v.min(i32::MAX as u32 / 60) as i32) * 60),
                print_name: snapshot.print.job_name,
                layer: snapshot.print.current_layer.map(|v| v.min(i32::MAX as u32) as i32),
                num_layers: snapshot.print.total_layers.map(|v| v.min(i32::MAX as u32) as i32),
                stage: snapshot.print.stage_code,
                print_error: snapshot.print_error_code,
                hms_errors: snapshot.system_error_codes,
                num_extruders: snapshot.num_extruders,
                slots_sets,
                slot_set_display_groups,
            };
            printers_info.push(printer_info);
        }
        printers_info
    }

    pub fn request_printer_command(&self, printer_serial: &str, command: BambuPrintCommand) -> Result<(), String> {
        let command = PrinterCommand::PrintControl(match command {
            BambuPrintCommand::Pause => PrintControlCommand::Pause,
            BambuPrintCommand::Resume => PrintControlCommand::Resume,
            BambuPrintCommand::Stop => PrintControlCommand::Stop,
        });
        let printer_id = BambuPrinterConfig::printer_id_for_serial(printer_serial);

        self.printer_manager
            .borrow_mut()
            .dispatch_by_id(&printer_id, command)
            .map_err(|err| format!("{err:?}"))
    }

    fn handle_printer_event(&self, event: PrinterEvent) {
        let PrinterEvent { printer_id, kind } = event;
        match kind {
            PrinterEventKind::ConnectivityChanged { connected } => self.handle_printer_connectivity_changed(&printer_id, connected),
            PrinterEventKind::SnapshotChanged { change, snapshot } => self.handle_printer_snapshot_changed(&printer_id, &change, snapshot.as_ref()),
            PrinterEventKind::SlotTagScanned {
                slot_id,
                tag_id,
                only_spool_id,
            } => self.handle_slot_tag_scanned(&printer_id, &slot_id, &tag_id, only_spool_id),
            PrinterEventKind::MaterialSlotPresenceChanged { changes } => self.handle_material_slot_presence_changed(&printer_id, changes),
        }
    }

    fn handle_printer_snapshot_changed(&self, printer_id: &PrinterId, change: &PrinterChange, snapshot: &printer_domain::PrinterSnapshot) {
        if !matches!(change, PrinterChange::All | PrinterChange::Slots) {
            return;
        }

        let Some(manager_index) = self.printer_manager.borrow().index_by_id(printer_id) else {
            error!("Snapshot event for unknown printer {}", printer_id.as_str());
            return;
        };
        if self.selected_manager_index() != Some(manager_index) {
            return;
        }

        self.update_slot_groups_from_snapshot(snapshot);
    }

    fn handle_printer_connectivity_changed(&self, printer_id: &PrinterId, connected: bool) {
        let Some(manager_index) = self.printer_manager.borrow().index_by_id(printer_id) else {
            error!("Connectivity event for unknown printer {}", printer_id.as_str());
            return;
        };
        let Some(ui_index) = self.ui_index_for_manager_index(manager_index) else {
            error!("Connectivity event for printer without UI row {}", printer_id.as_str());
            return;
        };

        let ui_borrow = self.ui_weak.unwrap();
        let ui = ui_borrow.global::<crate::app::AppState>();
        let ui_printers = ui.get_available_printers();
        let Some(mut printer_row) = ui_printers.row_data(ui_index as usize) else {
            error!("Connectivity event for missing UI row {}", ui_index);
            return;
        };
        printer_row.connected = connected;
        ui_printers.set_row_data(ui_index as usize, printer_row);

        let printer_number = self
            .printer_manager
            .borrow()
            .printer_number_by_id(printer_id)
            .unwrap_or(manager_index + 1);

        if connected {
            term_info!(&"-".repeat(62));
            term_info!("Printer [{}] connected successfully", printer_number);
            term_info!(&"-".repeat(62));
        } else {
            term_info!("[{}] Printer disconnected", printer_number);
        }
    }

    fn handle_slot_tag_scanned(&self, printer_id: &PrinterId, slot_id: &SlotId, tag_id: &str, only_spool_id: bool) {
        let Some(manager_index) = self.printer_manager.borrow().index_by_id(printer_id) else {
            error!("Tag scan event for unknown printer {}", printer_id.as_str());
            return;
        };
        if let Some(spool_id) = self.store.get_spool_id_by_tag_id(tag_id) {
            let printer_number = self
                .printer_manager
                .borrow()
                .printer_number_at(manager_index)
                .unwrap_or(manager_index + 1);
            info!(
                "[{}] Tag is registered, setting slot's spool-id{}",
                printer_number,
                if only_spool_id { "" } else { " and configuring slot material/color/k" }
            );
            let _ = self.dispatch_async_task(AppAsyncTaskRequest::ConfigureSlotWithSpool {
                printer_id: printer_id.clone(),
                slot_id: slot_id.clone(),
                spool_id,
                only_spool_id,
            });
        }
    }

    fn handle_material_slot_presence_changed(&self, printer_id: &PrinterId, changes: Vec<MaterialSlotPresenceChange>) {
        let _ = self.dispatch_async_task(AppAsyncTaskRequest::HandleMaterialSlotPresenceChanged {
            printer_id: printer_id.clone(),
            changes,
        });
    }

    fn slot_sets_from_snapshot(&self, snapshot: &printer_domain::PrinterSnapshot) -> Vec<SlotSet> {
        snapshot
            .slot_groups
            .iter()
            .map(|group| SlotSet {
                id: group.id.clone(),
                kind: Self::legacy_slot_group_kind(group.kind),
                name: group.name.clone(),
                short_name: group.short_name.clone(),
                driver_info: group.driver_info.clone(),
                slots: group.slots.iter().map(|slot| self.slot_snapshot_str(slot)).collect(),
                temp: group.temperature_c,
                humidity: group.humidity_percent,
            })
            .collect()
    }

    fn slot_set_display_groups_from_snapshot(snapshot: &printer_domain::PrinterSnapshot) -> Vec<SlotSetDisplayGroup> {
        snapshot
            .slot_group_display_groups
            .iter()
            .map(|display_group| SlotSetDisplayGroup {
                id: display_group.id.clone(),
                name: display_group.name.clone(),
                short_name: display_group.short_name.clone(),
                slot_set_ids: display_group.slot_group_ids.clone(),
            })
            .collect()
    }

    fn legacy_slot_group_kind(kind: printer_domain::SlotGroupKind) -> SpoolsSlotsKind {
        match kind {
            printer_domain::SlotGroupKind::Mms => SpoolsSlotsKind::Ams,
            _ => SpoolsSlotsKind::Ext,
        }
    }

    fn slot_snapshot_str(&self, slot: &printer_domain::MaterialSlotSnapshot) -> String {
        let (material, color) = match &slot.filament {
            printer_domain::PrinterFilament::Known(filament_info) => (filament_info.material_type.as_str(), filament_info.color_codes.join(";")),
            printer_domain::PrinterFilament::Unknown => ("", String::new()),
        };
        format!(
            "{},{},{},{},{},{},{}",
            Self::legacy_slot_state(slot.state),
            material,
            color,
            slot.pressure_advance_value.as_str(),
            slot.spool_id.as_deref().unwrap_or(""),
            self.weight_display_snapshot(slot),
            i32::from(slot.used_in_print),
        )
    }

    fn legacy_slot_state(state: printer_domain::SlotState) -> &'static str {
        match state {
            printer_domain::SlotState::Unknown => "Unknown",
            printer_domain::SlotState::Empty => "Empty",
            printer_domain::SlotState::Occupied => "Spool",
            printer_domain::SlotState::Reading => "Reading",
            printer_domain::SlotState::Ready => "Ready",
            printer_domain::SlotState::Loading => "Loading",
            printer_domain::SlotState::Unloading => "Unloading",
            printer_domain::SlotState::Loaded => "Loaded",
            printer_domain::SlotState::Error => "Error",
        }
    }

    fn print_state_from_snapshot(state: printer_domain::PrintState) -> PrintState {
        match state {
            printer_domain::PrintState::Unknown => PrintState::Unknown,
            printer_domain::PrintState::Idle => PrintState::Idle,
            printer_domain::PrintState::Slicing => PrintState::Slicing,
            printer_domain::PrintState::Preparing => PrintState::Prepare,
            printer_domain::PrintState::Printing => PrintState::Running,
            printer_domain::PrintState::Paused => PrintState::Pause,
            printer_domain::PrintState::Finished => PrintState::Finish,
            printer_domain::PrintState::Failed => PrintState::Failed,
            printer_domain::PrintState::Canceled => PrintState::Unknown,
        }
    }

    fn weight_display_snapshot(&self, slot: &printer_domain::MaterialSlotSnapshot) -> String {
        if let Some(weight_left) = self.weight_left_snapshot(slot) {
            format!("{:.1}g", weight_left)
        } else if slot.consumed_since_load_g != 0.0 {
            format!("-{:.1}g", slot.consumed_since_load_g)
        } else {
            String::new()
        }
    }

    fn weight_left_snapshot(&self, slot: &printer_domain::MaterialSlotSnapshot) -> Option<f32> {
        let spool_id = slot.spool_id.as_ref()?;
        let spool = self.store.get_spool_by_id(spool_id)?;
        self.weight_left_spool(&spool, Some(slot.consumed_since_weight_g))
    }
}

impl printer_domain::PrinterObserver for ViewModel {
    fn on_printer_event(&mut self, event: PrinterEvent) {
        self.handle_printer_event(event);
    }
}

// TODO:
// Add support for technical PN532 severe errors reporting (when can't connect to device, etc.)
impl SpoolTagObserver for ViewModel {
    fn on_tag_status(&mut self, status: &Status) {
        if !matches!(status, Status::Failure(spool_tag::Failure::TagReadFailure)) {
            // TagReadFailure separately without other events before could be just random error with PN532
            // and are ignored on the UI (since need an evenr prior to switch to read/write state, only then errors are considered)
            // So no point in turning on display
            // Might even make more sense to control undimming display from the slint code and not from here

            self.framework.borrow().undim_display();
        }
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
                    if let Some(mut spool_rec) = self.store.get_spool_by_id(&encode_cookie.spool_rec_id).map(Box::new) {
                        spool_rec.encode_time = encode_cookie.encode_time;
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::UpdateSpoolRec {
                            spool_rec,
                            message_box: None,
                        });
                    }
                } else if let Ok(encode_cookie) = serde_json::from_str::<LocationEncodeCookie>(cookie) {
                    debug!(">>>> TODO: Store the tag into DB with Tag Id, descriptor: {_encoded_descriptor}");
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
                spool_tag::ReadResult::TagInStore { uid } => {
                    // Handling of tag in store, same as below
                    debug!("Scanned Tag which is in store");
                    let hex_tag = hex::encode_upper(uid);
                    if let Some(spool_rec) = self.store.get_spool_by_hex_tag(&hex_tag) {
                        self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Scanned);
                        self.filament_staging.borrow_mut().set_scanned_tag_id(Some(hex_tag));
                        self.display_filament_staging(true);
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
                    } else {
                        error!("Tag scanned as in store, not found in store");
                    }
                }
                spool_tag::ReadResult::NDEF { uid, message } => {
                    let hex_tag = hex::encode_upper(uid);
                    // Check if it is a known tag
                    if let Some(spool_rec) = self.store.get_spool_by_hex_tag(&hex_tag) {
                        debug!("Scanned Tag which is in store");
                        // Handling of tag in store, same as above
                        self.filament_staging.borrow_mut().set_spool_record(spool_rec, StagingOrigin::Scanned);
                        self.filament_staging.borrow_mut().set_scanned_tag_id(Some(hex_tag));
                        self.display_filament_staging(true);
                        let _ = self.dispatch_async_task(AppAsyncTaskRequest::SetStagingRecExt {});
                    } else {
                        // Not known
                        // Check if some special format
                        if let Some(ndef_bytes) = message
                            && let Ok(ndef) = NdefMessage::decode(ndef_bytes)
                        {
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
                        self.filament_staging.borrow_mut().set_scanned_tag_id(Some(hex_tag));
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
        self.app_config.borrow_mut().set_redirect_web_to_inventory();
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
            &format!("{ip_url}/config\n{name_url}/config")
        } else {
            &format!("{ip_url}/config")
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
    fn on_tags_changed(&self) {
        let tags_in_store = self.store.tags_in_store();
        let _ = self.spool_scale_model.borrow().tags_in_store(tags_in_store);
    }

    fn on_store_error(&self, detail: &str) {
        self.message_box(
            "Data Store Error",
            "SD card issues, data store unavailable",
            detail,
            crate::app::StatusType::Error,
            -2,
        );
    }
}

fn get_brand_from_text(text: &str) -> Option<&'static str> {
    let text = text.to_lowercase();
    // prioritize start with
    for brand in FILAMENT_BRAND_NAMES.lines() {
        if brand.contains(',') {
            if let Some((keyword, brand)) = brand.split_once(',')
                && text.starts_with(&keyword.to_lowercase())
            {
                return Some(brand);
            }
        } else if text.starts_with(&brand.to_lowercase()) {
            return Some(brand);
        }
    }
    // if not found continue to contains
    for brand in FILAMENT_BRAND_NAMES.lines() {
        if brand.contains(',') {
            if let Some((keyword, brand)) = brand.split_once(',')
                && text.contains(&keyword.to_lowercase())
            {
                return Some(brand);
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

fn validate_persistent_printer_state_path(view_model: &Rc<RefCell<ViewModel>>, manager_index: usize, path: &str) -> Result<(), String> {
    if !path.starts_with("/state/") || path.contains("..") || path.contains("//") {
        return Err(format!("Invalid printer state path {path}"));
    }

    let duplicate = {
        let view_model_borrow = view_model.borrow();
        view_model_borrow
            .printer_manager
            .borrow()
            .persistent_state_paths()
            .into_iter()
            .find(|(other_index, _printer_id, other_path)| *other_index != manager_index && other_path == path)
    };
    if let Some((_other_index, other_printer_id, _path)) = duplicate {
        return Err(format!("Printer state path {path} is also used by {}", other_printer_id.as_str()));
    }

    Ok(())
}

fn bad_persistent_printer_state_path(path: &str) -> String {
    let last_slash = path.rfind('/').unwrap_or_default();
    if let Some(last_dot) = path.rfind('.')
        && last_dot > last_slash
    {
        return format!("{}.bad", &path[..last_dot]);
    }
    format!("{path}.bad")
}

async fn quarantine_bad_persistent_printer_state(framework: &Rc<RefCell<Framework>>, path: &str, state_str: &str) {
    let bad_path = bad_persistent_printer_state_path(path);
    let file_store = framework.borrow().file_store();
    let mut file_store = file_store.lock().await;
    match file_store.create_write_file_str(&bad_path, state_str).await {
        Ok(()) => {
            if let Err(err) = file_store.delete_file(path).await {
                error!("Failed to delete bad printer state file {path}: {err}");
            }
        }
        Err(err) => error!("Failed to write bad printer state backup {bad_path}: {err}"),
    }
}

async fn load_persistent_printer_state(
    framework: &Rc<RefCell<Framework>>,
    view_model: &Rc<RefCell<ViewModel>>,
    store: &Rc<Store>,
    manager_index: usize,
) -> Result<bool, String> {
    let (printer_number, path) = {
        let view_model_borrow = view_model.borrow();
        let printer_manager = view_model_borrow.printer_manager.borrow();
        let Some(printer_number) = printer_manager.printer_number_at(manager_index) else {
            return Err(format!("Unknown printer index {manager_index}"));
        };
        let Some(path) = printer_manager.persistent_state_path_at(manager_index) else {
            return Ok(false);
        };
        (printer_number, path)
    };
    validate_persistent_printer_state_path(view_model, manager_index, &path)?;

    let mut err_str = String::new();
    for trial in 1..=3 {
        let state_str = {
            let file_store = framework.borrow().file_store();
            let mut file_store = file_store.lock().await;
            match file_store.read_file_str(&path).await {
                Ok(state_str) => state_str,
                Err(err) => {
                    term_error!(
                        "[{}] Can't read printer state file (ok, if first printer run) {} : {}",
                        printer_number,
                        path,
                        err
                    );
                    return Ok(false);
                }
            }
        };

        if state_str.trim().is_empty() {
            err_str = format!("[{}] Loaded empty state file {} in trial {}", printer_number, path, trial);
            term_error!("{}", err_str);
            Timer::after_millis(250).await;
            continue;
        }

        if let Err(err) = serde_json::from_str::<printer_domain::PrinterStateFile>(&state_str) {
            quarantine_bad_persistent_printer_state(framework, &path, &state_str).await;
            return Err(format!("Failed to parse printer state: {err}"));
        }

        view_model
            .borrow()
            .printer_manager
            .borrow_mut()
            .load_persistent_state_at(manager_index, &state_str, store)?;
        term_info!("[{}] Restored printer state from SDCard", printer_number);
        return Ok(true);
    }

    Err(err_str)
}

fn refresh_selected_printer_after_state_restore(view_model: &Rc<RefCell<ViewModel>>, manager_index: usize) {
    let selected_manager_index = {
        let view_model_borrow = view_model.borrow();
        view_model_borrow.selected_manager_index()
    };
    if selected_manager_index != Some(manager_index) {
        return;
    }

    view_model.borrow().update_slot_groups_from_selected_printer();
}

async fn restore_printer_runtime_states(framework: &Rc<RefCell<Framework>>, view_model: &Rc<RefCell<ViewModel>>, num_of_printers: usize) {
    for manager_index in 0..num_of_printers {
        let restore_future = {
            view_model
                .borrow()
                .printer_manager
                .borrow_mut()
                .prepare_runtime_state_restore_at(manager_index, framework.clone())
        };
        match restore_future {
            Ok(Some(restore_future)) => match restore_future.await {
                Ok(()) => refresh_selected_printer_after_state_restore(view_model, manager_index),
                Err(err) => view_model.borrow().message_box(
                    "Restore Running Print State Notice",
                    &err,
                    "Print Tracking Can't be Resumed",
                    crate::app::StatusType::Error,
                    0,
                ),
            },
            Ok(None) => {}
            Err(err) => error!("Error preparing printer runtime state restore: {err}"),
        }
    }
}

fn restore_persistent_printer_dirty_state(view_model: &Rc<RefCell<ViewModel>>, manager_index: usize) {
    if let Err(err) = view_model
        .borrow()
        .printer_manager
        .borrow_mut()
        .restore_persistent_state_after_failed_store_at(manager_index)
    {
        error!("Failed to restore printer dirty state after store failure: {err}");
    }
}

async fn store_persistent_printer_state(
    framework: &Rc<RefCell<Framework>>,
    view_model: &Rc<RefCell<ViewModel>>,
    manager_index: usize,
) -> Result<bool, String> {
    let payload = view_model
        .borrow()
        .printer_manager
        .borrow_mut()
        .prepare_persistent_state_store_at(manager_index)?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let printer_number = view_model
        .borrow()
        .printer_manager
        .borrow()
        .printer_number_at(manager_index)
        .unwrap_or(manager_index + 1);

    if let Err(err) = validate_persistent_printer_state_path(view_model, manager_index, &payload.path) {
        restore_persistent_printer_dirty_state(view_model, manager_index);
        return Err(err);
    }
    if payload.contents.trim().is_empty() {
        restore_persistent_printer_dirty_state(view_model, manager_index);
        return Err(format!("Printer state to store is empty for {}", payload.path));
    }

    info!("[{printer_number}] Storing printer state to {}", payload.path);
    let file_store = framework.borrow().file_store();
    let mut file_store = file_store.lock().await;
    match file_store.create_write_file_str(&payload.path, &payload.contents).await {
        Ok(_) => match file_store.read_file_str(&payload.path).await {
            Ok(verify_read_str) => {
                if verify_read_str == payload.contents {
                    info!("[{printer_number}] Store state verification passed for {}", payload.path);
                    view_model
                        .borrow()
                        .printer_manager
                        .borrow_mut()
                        .persistent_state_store_succeeded_at(manager_index)?;
                    Ok(true)
                } else {
                    restore_persistent_printer_dirty_state(view_model, manager_index);
                    error!(
                        "[{printer_number}] During store state verification read data differ from written data for {}",
                        payload.path
                    );
                    Err(String::from("Verification of state store failed"))
                }
            }
            Err(err) => {
                restore_persistent_printer_dirty_state(view_model, manager_index);
                error!("[{printer_number}] Failed to verify store printer restart state {} : {err}", payload.path);
                Err(format!("Error reading state store to verify : {err}"))
            }
        },
        Err(err) => {
            restore_persistent_printer_dirty_state(view_model, manager_index);
            error!("[{printer_number}] Failed to store printer restart state {} : {err}", payload.path);
            Err(format!("Error storing state : {err}"))
        }
    }
}

fn runtime_persistence_error_text(request_kind: PrinterRuntimePersistenceRequestKind) -> Option<&'static str> {
    match request_kind {
        PrinterRuntimePersistenceRequestKind::StorePrintProject => Some("SpoolEase Will Not be Able to Resume Tracking if Restarted"),
        PrinterRuntimePersistenceRequestKind::StoreConsumeIndex { .. } => {
            Some("If This Error Repeats, SpoolEase Will Not be Able to Resume Tracking if Restarted")
        }
        PrinterRuntimePersistenceRequestKind::DeletePrintProject => None,
    }
}

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
    while !store.is_available() {
        Timer::after_millis(100).await;
    }
    Timer::after_millis(250).await;

    let num_of_printers = view_model.borrow().printer_manager.borrow().len();
    if num_of_printers == 0 {
        return;
    }

    term_info!("Restoring printer(s) state");
    for manager_index in 0..num_of_printers {
        match load_persistent_printer_state(&framework, &view_model, &store, manager_index).await {
            Ok(true) => refresh_selected_printer_after_state_restore(&view_model, manager_index),
            Ok(false) => {}
            Err(err) => view_model
                .borrow()
                .message_box("Restore Print State Notice", &err, "", crate::app::StatusType::Error, 0),
        }
    }
    restore_printer_runtime_states(&framework, &view_model, num_of_printers).await;

    let mut printer_index = 0;
    let delay_time = max(1000u64, (3000 / num_of_printers) as u64); // want every printer to save every 3 seconds, and not all together
    let runtime_persistence_request_channel = view_model.borrow().runtime_persistence_request_channel.clone();
    let receiver = runtime_persistence_request_channel.receiver();
    loop {
        // Timer::after_millis(delay_time).await;
        match with_timeout(Duration::from_millis(delay_time), receiver.receive()).await {
            Ok(request) => {
                let request_kind = request.kind;
                let persistence_future = {
                    view_model
                        .borrow()
                        .printer_manager
                        .borrow_mut()
                        .prepare_runtime_persistence_request_by_id(&request.printer_id, framework.clone(), request_kind)
                };
                match persistence_future {
                    Ok(Some(persistence_future)) => {
                        if let Err(err) = persistence_future.await
                            && let Some(text2) = runtime_persistence_error_text(request_kind)
                        {
                            view_model
                                .borrow()
                                .message_box("Print Tracking Notice", &err, text2, crate::app::StatusType::Error, 0);
                        }
                    }
                    Ok(None) => {}
                    Err(err) => error!("Error preparing printer runtime persistence request: {err}"),
                }
            }
            Err(_) => {
                // Time expired
                if printer_index < num_of_printers {
                    let num_retries = 3;
                    for retry in 1..=num_retries {
                        match store_persistent_printer_state(&framework, &view_model, printer_index).await {
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
                                    let printer_number = view_model
                                        .borrow()
                                        .printer_manager
                                        .borrow()
                                        .printer_number_at(printer_index)
                                        .unwrap_or(printer_index + 1);
                                    error!("[{printer_number}] Failed all retries trying to store printer restart state : {err}");
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

struct PendingPrinterConsumption {
    printer_id: PrinterId,
    slot_id: SlotId,
    spool_id: String,
    consumed_since_load_g: f32,
    consumed_since_load_saved_g: f32,
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
    loop {
        let pending_consumption = {
            let view_model_borrow = view_model.borrow();
            let printer_manager = view_model_borrow.printer_manager.borrow();
            (0..printer_manager.len())
                .filter_map(|printer_index| printer_manager.snapshot_state_at(printer_index))
                .flat_map(|snapshot_state| {
                    snapshot_state.with_snapshot(|snapshot| {
                        let printer_id = snapshot.id.clone();
                        snapshot
                            .slot_groups
                            .iter()
                            .flat_map(move |group| {
                                let printer_id = printer_id.clone();
                                group.slots.iter().filter_map(move |slot| {
                                    let spool_id = slot.spool_id.clone()?;
                                    if slot.consumed_since_load_g == 0.0 || slot.consumed_since_load_saved_g == slot.consumed_since_load_g {
                                        return None;
                                    }
                                    Some(PendingPrinterConsumption {
                                        printer_id: printer_id.clone(),
                                        slot_id: slot.id.clone(),
                                        spool_id,
                                        consumed_since_load_g: slot.consumed_since_load_g,
                                        consumed_since_load_saved_g: slot.consumed_since_load_saved_g,
                                    })
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>()
        };

        for pending in pending_consumption {
            let store = view_model.borrow().store.clone();
            if let Some(mut spool_rec) = store.get_spool_by_id(&pending.spool_id) {
                let consumption_to_add_save = pending.consumed_since_load_g - pending.consumed_since_load_saved_g;
                spool_rec.consumed_since_add += consumption_to_add_save;
                spool_rec.consumed_since_weight += consumption_to_add_save;
                info!(
                    "Increase spool {} consumption by {:2}g to total so far {:2}g and since last weight to {:2}g",
                    pending.spool_id, consumption_to_add_save, spool_rec.consumed_since_add, spool_rec.consumed_since_weight
                );
                match store.update_spool(spool_rec, None).await {
                    Ok(_) => {
                        if let Err(err) = view_model.borrow().printer_manager.borrow_mut().acknowledge_slot_consumption_saved_by_id(
                            &pending.printer_id,
                            &pending.slot_id,
                            pending.consumed_since_load_g,
                        ) {
                            error!(
                                "Error acknowledging consumption for printer {} slot {}: {:?}",
                                pending.printer_id.as_str(),
                                pending.slot_id.as_str(),
                                err
                            );
                        }
                    }
                    Err(err) => {
                        error!("Error updating consumption of spool {} : {err}", pending.spool_id);
                    }
                }
            } else {
                error!("While updating consume data spool_id not found");
            }
        }
        Timer::after_secs(1).await;
    }
}

#[derive(Debug, Clone)]
struct MessageBox {
    title: String,
    text: String,
    text2: String,
    timeout: i32,
}

#[derive(Debug, Clone, Copy)]
enum LinkTagMode {
    ToUntaggedSpool,
    ToTaggedSpool,
}

#[derive(Debug, Clone, Copy)]
enum UnlinkTagMode {
    AllTags,
    ScannedTag,
}

#[derive(Debug, Clone)]
enum AppAsyncTaskRequest {
    LinkTagToSpool {
        tag_id: String,
        tag_type: String,
        spool_id: String,
        mode: LinkTagMode,
        final_step: bool,
    },
    UnLinkSpoolTags {
        spool_id: String,
        mode: UnlinkTagMode,
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
        spool_rec: Box<SpoolRecord>,
        message_box: Option<MessageBox>,
    },
    ConfigureSlotWithSpool {
        printer_id: PrinterId,
        slot_id: SlotId,
        spool_id: String,
        only_spool_id: bool,
    },
    HandleMaterialSlotPresenceChanged {
        printer_id: PrinterId,
        changes: Vec<MaterialSlotPresenceChange>,
    },
    ImportDefinitionTagToInventory {
        tag_definition_type: String,
        tag_definition_info: String,
        empty_spool_weight: i32,
        spool_is_full: bool,
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
            AppAsyncTaskRequest::LinkTagToSpool {
                tag_id,
                tag_type,
                spool_id,
                mode,
                final_step,
            } => ViewModel::link_tag_to_spool_id_async(view_model.clone(), tag_id, tag_type, spool_id, mode, final_step).await,
            AppAsyncTaskRequest::UnLinkSpoolTags { spool_id, mode } => ViewModel::unlink_spool_tags_async(view_model.clone(), spool_id, mode).await,
            AppAsyncTaskRequest::SetStagingRecExt {} => ViewModel::set_staging_rec_ext_async(view_model.clone()).await,
            AppAsyncTaskRequest::SetSpoolWeight {
                spool_id,
                weight_current,
                weight_new,
                final_step,
                from_button,
            } => ViewModel::set_spool_weight_async(view_model.clone(), spool_id, weight_current, weight_new, final_step, from_button).await,
            AppAsyncTaskRequest::UpdateSpoolRec { spool_rec, message_box } => {
                ViewModel::update_spool_rec_async(view_model.clone(), spool_rec, message_box).await
            }
            AppAsyncTaskRequest::ConfigureSlotWithSpool {
                printer_id,
                slot_id,
                spool_id,
                only_spool_id,
            } => ViewModel::configure_slot_with_spool_async(view_model.clone(), printer_id, slot_id, spool_id, only_spool_id).await,
            AppAsyncTaskRequest::HandleMaterialSlotPresenceChanged { printer_id, changes } => {
                ViewModel::handle_material_slot_presence_changed_async(view_model.clone(), printer_id, changes).await
            }
            AppAsyncTaskRequest::ImportDefinitionTagToInventory {
                tag_definition_type,
                tag_definition_info,
                empty_spool_weight,
                spool_is_full,
            } => {
                ViewModel::import_definition_tag_to_inventory_async(
                    view_model.clone(),
                    tag_definition_type,
                    tag_definition_info,
                    empty_spool_weight,
                    spool_is_full,
                )
                .await
            }
        }
    }
}
