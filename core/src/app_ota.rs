use alloc::string::{String, ToString};
use core::cell::RefCell;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use framework::framework::FrameworkObserver;
use framework::ota::{OtaObserver, OtaRequest, run_ota};
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
}

impl OtaObserver for AppOtaObserver {
    fn on_ota_start(&mut self) {
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
                };
                if product == AppOtaProduct::Console {
                    let curr_ver = framework.borrow().settings.app_cargo_pkg_version.to_string();
                    let index = match train {
                        AppOtaTrain::Stable => 0,
                        AppOtaTrain::Unstable => 1,
                        AppOtaTrain::Debug => 2,
                    };
                    let ota_item = &mut ota_info[index];
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
                } else {
                    error!("Internal Error: Scale firmware update should run on scale");
                }
            }
        }
    }
}
