use alloc::format;
use alloc::string::{String, ToString};
use core::cell::RefCell;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use framework::framework::FrameworkObserver;
use framework::ota::{
    APP_OTA_ACTION_INSTALL, APP_OTA_ACTION_PATH, APP_OTA_ACTION_STAGING, APP_OTA_BINARY_PATH, APP_OTA_METADATA_PATH, OtaObserver, OtaPartitionLayout,
    OtaPhase, OtaRequest, activate_ota_partition, download_ota_to_sd, ota_partition_layout, run_ota,
};
use framework::{error, info};
use shared::settings::{
    OTA_DOMAIN_DEBUG, OTA_DOMAIN_STABLE, OTA_DOMAIN_UNSTABLE, OTA_TLS_CERTIFICATE, SCALE_DEBUG_OTA_PATH, SCALE_STABLE_OTA_PATH,
    SCALE_UNSTABLE_OTA_PATH,
};
use shared::types::AppOtaTrain;

use alloc::rc::Rc;
use framework::prelude::Framework;

use crate::settings::{CONSOLE_DEBUG_OTA_PATH, CONSOLE_STABLE_OTA_PATH, CONSOLE_UNSTABLE_OTA_PATH};
use crate::view_model::ViewModel;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum ConsoleOtaMode {
    Auto,
    Direct,
    SdStaged,
}

// Keep both implementations in one binary. Auto uses the installed partition layout, while the
// explicit modes are useful for controlled development tests without introducing feature flags.
const CONSOLE_OTA_MODE: ConsoleOtaMode = ConsoleOtaMode::Auto;
const MIB: u32 = 1024 * 1024;

fn resolve_console_ota_mode(layout: OtaPartitionLayout) -> Result<ConsoleOtaMode, String> {
    match CONSOLE_OTA_MODE {
        ConsoleOtaMode::Direct => Ok(ConsoleOtaMode::Direct),
        ConsoleOtaMode::SdStaged => Ok(ConsoleOtaMode::SdStaged),
        ConsoleOtaMode::Auto => {
            let smaller = layout.ota_0.size.min(layout.ota_1.size) as u64;
            let larger = layout.ota_0.size.max(layout.ota_1.size) as u64;

            // Slots within 25% of each other are treated as interchangeable A/B partitions.
            // A dedicated updater layout requires ota_1 to be at least 1 MiB and no more than
            // half of ota_0. The gap between these rules is intentional: unfamiliar layouts fail
            // closed instead of selecting a potentially destructive update strategy.
            if smaller * 4 >= larger * 3 {
                Ok(ConsoleOtaMode::Direct)
            } else if layout.ota_1.size >= MIB && (layout.ota_1.size as u64) * 2 <= layout.ota_0.size as u64 {
                Ok(ConsoleOtaMode::SdStaged)
            } else {
                Err(format!(
                    "Unsupported OTA layout: ota_0={} bytes, ota_1={} bytes",
                    layout.ota_0.size, layout.ota_1.size
                ))
            }
        }
    }
}

async fn write_app_action(framework: Rc<RefCell<Framework>>, action: &str) -> Result<(), String> {
    let file_store = framework.borrow().file_store();
    let mut file_store = file_store.lock().await;
    if !file_store.card_installed {
        return Err(String::from("SD card is unavailable"));
    }
    file_store
        .create_write_file_str(APP_OTA_ACTION_PATH, &format!("{action}\n"))
        .await
        .map_err(|err| format!("Failed writing {APP_OTA_ACTION_PATH}: {err}"))
}

#[allow(clippy::too_many_arguments)]
async fn run_sd_staged_console_ota(
    ota_domain: &str,
    ota_path: &str,
    cur_version: &str,
    framework: Rc<RefCell<Framework>>,
    observer: &mut dyn OtaObserver,
    layout: OtaPartitionLayout,
) -> Result<(), String> {
    if layout.running != Some(0) {
        return Err(format!(
            "SD-staged Console OTA requires Console in ota_0; running partition is {:?}",
            layout.running
        ));
    }
    if layout.ota_1.size < MIB {
        return Err(format!("Updater partition is too small: {} bytes", layout.ota_1.size));
    }

    // APP.ACT is the commit journal. STAGING is written before either payload file changes, and
    // INSTALL is written only after APP.BIN and APP.TOM are complete and verified.
    write_app_action(framework.clone(), APP_OTA_ACTION_STAGING).await?;
    let metadata = download_ota_to_sd(
        ota_domain,
        ota_path,
        "ota.toml",
        cur_version,
        OTA_TLS_CERTIFICATE,
        APP_OTA_BINARY_PATH,
        APP_OTA_METADATA_PATH,
        layout.ota_0.size,
        framework.clone(),
        observer,
    )
    .await?;
    write_app_action(framework.clone(), APP_OTA_ACTION_INSTALL).await?;

    observer.on_ota_status("Activating updater");
    activate_ota_partition(1)?;
    observer.on_ota_completed(&format!("{} staged successfully. Restarting into updater...", metadata.filename));
    Timer::after_secs(2).await;
    Framework::reset_device_safer_async(framework, Some(Duration::from_secs(5))).await;
    Ok(())
}

#[derive(Debug, PartialEq, Default)]
pub enum AppOtaProduct {
    #[default]
    Console,
    Scale,
}

#[derive(Debug)]
pub enum AppOtaRequest {
    CheckOta,
    Update { product: AppOtaProduct, train: AppOtaTrain },
}

struct AppOtaObserver {
    update: bool,
    view_model: Rc<RefCell<ViewModel>>,
    framework: Rc<RefCell<Framework>>,
    pub version: String,
    pub newer: bool,
    pub notify_framework: bool,
    progress_percent: u8,
    ota_mode: Option<ConsoleOtaMode>,
}

impl OtaObserver for AppOtaObserver {
    fn on_ota_start(&mut self) {
        self.progress_percent = 0;
        if self.update {
            self.view_model.borrow_mut().on_ota_start();
        } else {
            self.view_model.borrow_mut().on_ota_status("Checking for firmware");
        }
        // if self.notify_framework {
        //     self.framework.borrow_mut().notify_ota_start();
        // }
    }

    fn on_ota_status(&mut self, text: &str) {
        self.view_model.borrow_mut().on_ota_status(text);
    }

    fn on_ota_failed(&mut self, text: &str) {
        self.view_model.borrow_mut().on_ota_failed(text);
        if self.notify_framework {
            self.framework.borrow_mut().notify_ota_failed(text);
        }
    }

    fn on_ota_completed(&mut self, text: &str) {
        if self.update {
            self.view_model.borrow_mut().on_ota_completed(text);
        } else {
            self.view_model.borrow_mut().on_ota_status("Processing firmware information");
        }
        // if self.notify_framework {
        //     self.framework.borrow_mut().notify_ota_completed(text);
        // }
    }

    fn on_ota_version_available(&mut self, version: &str, newer: bool) {
        self.version = version.to_string();
        self.newer = newer;
        if self.newer {
            self.view_model.borrow_mut().on_ota_version_available(version, newer);
        }
        if self.notify_framework {
            self.framework.borrow_mut().notify_ota_version_available(version, newer);
        }
    }

    fn on_ota_progress(&mut self, phase: OtaPhase, filename: &str, completed: u32, total: u32) {
        let percent = if total == 0 {
            0
        } else {
            ((completed as u64 * 100) / total as u64) as u8
        };
        if percent == self.progress_percent {
            return;
        }
        self.progress_percent = percent;
        let phase = match phase {
            OtaPhase::Downloading if self.ota_mode == Some(ConsoleOtaMode::SdStaged) => {
                format!("Downloading {filename} to SD card")
            }
            OtaPhase::Downloading => format!("Downloading/flashing {filename}"),
            OtaPhase::Flashing => format!("Flashing {filename}"),
            OtaPhase::Completing => format!("Finalizing {filename}"),
        };
        self.view_model.borrow_mut().on_ota_status(&format!("{phase}: {percent}%"));
    }
}

#[derive(Debug, Default)]
pub struct FirmwareInfo {
    pub product: AppOtaProduct,
    pub train: AppOtaTrain,
    pub domain: &'static str,
    pub path: &'static str,
    pub version: String,
    pub newer: bool,
}

pub type AppOtaRequestChannel = Channel<NoopRawMutex, AppOtaRequest, 5>;
pub async fn app_ota_task(framework: Rc<RefCell<Framework>>, view_model: Rc<RefCell<ViewModel>>) {
    let app_ota_request_channel = view_model.borrow().app_ota_request_channel.clone();
    let receiver = app_ota_request_channel.receiver();
    let mut ota_info = alloc::vec![
        FirmwareInfo {
            product: AppOtaProduct::Console,
            train: AppOtaTrain::Stable,
            domain: OTA_DOMAIN_STABLE,
            path: CONSOLE_STABLE_OTA_PATH,
            ..Default::default()
        },
        FirmwareInfo {
            product: AppOtaProduct::Console,
            train: AppOtaTrain::Unstable,
            domain: OTA_DOMAIN_UNSTABLE,
            path: CONSOLE_UNSTABLE_OTA_PATH,
            ..Default::default()
        },
        FirmwareInfo {
            product: AppOtaProduct::Console,
            train: AppOtaTrain::Debug,
            domain: OTA_DOMAIN_DEBUG,
            path: CONSOLE_DEBUG_OTA_PATH,
            ..Default::default()
        },
        FirmwareInfo {
            product: AppOtaProduct::Scale,
            train: AppOtaTrain::Stable,
            domain: OTA_DOMAIN_STABLE,
            path: SCALE_STABLE_OTA_PATH,
            ..Default::default()
        },
        FirmwareInfo {
            product: AppOtaProduct::Scale,
            train: AppOtaTrain::Unstable,
            domain: OTA_DOMAIN_UNSTABLE,
            path: SCALE_UNSTABLE_OTA_PATH,
            ..Default::default()
        },
        FirmwareInfo {
            product: AppOtaProduct::Scale,
            train: AppOtaTrain::Debug,
            domain: OTA_DOMAIN_DEBUG,
            path: SCALE_DEBUG_OTA_PATH,
            ..Default::default()
        },
    ];
    loop {
        let ota_request = receiver.receive().await;
        match ota_request {
            AppOtaRequest::CheckOta => {
                let mut app_ota_observer = AppOtaObserver {
                    update: false,
                    view_model: view_model.clone(),
                    framework: framework.clone(),
                    version: String::new(),
                    newer: false,
                    notify_framework: false,
                    progress_percent: 0,
                    ota_mode: None,
                };

                for ota_item in ota_info.iter_mut() {
                    let curr_ver = match ota_item.product {
                        AppOtaProduct::Console => framework.borrow().settings.app_cargo_pkg_version.to_string(),
                        AppOtaProduct::Scale => view_model.borrow().scale_version.clone().unwrap_or_default(),
                    };

                    app_ota_observer.notify_framework = ota_item.product == AppOtaProduct::Console && ota_item.train == AppOtaTrain::Stable;

                    info!("---- Checking available firmware for {:?}-{:?} ----", ota_item.product, ota_item.train);
                    run_ota(
                        ota_item.domain,
                        ota_item.path,
                        "ota.toml",
                        &curr_ver,
                        OTA_TLS_CERTIFICATE,
                        OtaRequest::CheckVersion,
                        framework.clone(),
                        &mut app_ota_observer,
                    )
                    .await;
                    core::mem::swap(&mut app_ota_observer.version, &mut ota_item.version);
                    core::mem::swap(&mut app_ota_observer.newer, &mut ota_item.newer);
                }
                view_model.borrow().update_firmware_versions(&ota_info);
            }
            AppOtaRequest::Update { product, train } => {
                let mut app_ota_observer = AppOtaObserver {
                    update: true,
                    view_model: view_model.clone(),
                    framework: framework.clone(),
                    version: String::new(),
                    newer: false,
                    notify_framework: false,
                    progress_percent: 0,
                    ota_mode: None,
                };
                if product == AppOtaProduct::Console {
                    let curr_ver = framework.borrow().settings.app_cargo_pkg_version.to_string();
                    let index = match train {
                        AppOtaTrain::Stable => 0,
                        AppOtaTrain::Unstable => 1,
                        AppOtaTrain::Debug => 2,
                    };
                    let ota_item = &mut ota_info[index];
                    let layout = match ota_partition_layout() {
                        Ok(layout) => layout,
                        Err(message) => {
                            app_ota_observer.on_ota_failed(&message);
                            continue;
                        }
                    };
                    match resolve_console_ota_mode(layout) {
                        Ok(ConsoleOtaMode::Direct) => {
                            app_ota_observer.ota_mode = Some(ConsoleOtaMode::Direct);
                            run_ota(
                                ota_item.domain,
                                ota_item.path,
                                "ota.toml",
                                &curr_ver,
                                OTA_TLS_CERTIFICATE,
                                OtaRequest::Update,
                                framework.clone(),
                                &mut app_ota_observer,
                            )
                            .await;
                        }
                        Ok(ConsoleOtaMode::SdStaged) => {
                            app_ota_observer.ota_mode = Some(ConsoleOtaMode::SdStaged);
                            if let Err(message) = run_sd_staged_console_ota(
                                ota_item.domain,
                                ota_item.path,
                                &curr_ver,
                                framework.clone(),
                                &mut app_ota_observer,
                                layout,
                            )
                            .await
                            {
                                app_ota_observer.on_ota_failed(&message);
                            }
                        }
                        Ok(ConsoleOtaMode::Auto) => unreachable!(),
                        Err(message) => app_ota_observer.on_ota_failed(&message),
                    }
                } else {
                    error!("Internal Error: Scale firmware update should run on scale");
                }
            }
        }
    }
}
