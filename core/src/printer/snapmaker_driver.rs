use alloc::{
    rc::{Rc, Weak},
    string::{String, ToString},
    vec::Vec,
};
use core::{cell::RefCell, future::pending};

use crate::app_config::SnapmakerU1PrinterConfig;
use framework::utils::SpawnerHeapExt;
use framework::{info, prelude::Framework, term_info, warn};

use super::{
    PrinterCapabilities, PrinterCommand, PrinterDriver, PrinterDriverKind, PrinterError, PrinterId, PrinterObserver, PrinterResult, PrinterSnapshot,
    PrinterSnapshotState, PrinterSnapshotStateInner,
};

pub struct SnapmakerU1PrinterDriver {
    id: PrinterId,
    state: PrinterSnapshotState,
    config: SnapmakerU1PrinterConfig,
    printer_number: usize,
    task_started: bool,
}

impl SnapmakerU1PrinterDriver {
    pub fn new(name: Option<String>, config: &SnapmakerU1PrinterConfig, printer_number: usize) -> PrinterResult<Self> {
        let id = config.printer_id().map_err(PrinterError::InvalidCommand)?;
        let name = name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Snapmaker U1")
            .to_string();
        let snapshot = PrinterSnapshot {
            id: id.clone(),
            kind: PrinterDriverKind::SnapmakerU1,
            native_id: config.serial.as_deref().unwrap_or_default().trim().to_string(),
            name,
            connected: false,
            slot_groups_known: false,
            ..Default::default()
        };
        Ok(Self {
            id,
            state: Rc::new(PrinterSnapshotStateInner::new(snapshot)),
            config: config.clone(),
            printer_number,
            task_started: false,
        })
    }
}

impl PrinterDriver for SnapmakerU1PrinterDriver {
    fn id(&self) -> &PrinterId {
        &self.id
    }

    fn kind(&self) -> PrinterDriverKind {
        PrinterDriverKind::SnapmakerU1
    }

    fn display_name(&self) -> String {
        self.state.with_snapshot(|snapshot| snapshot.name.clone())
    }

    fn capabilities(&self) -> PrinterCapabilities {
        PrinterCapabilities::default()
    }

    fn snapshot_state(&self) -> PrinterSnapshotState {
        self.state.clone()
    }

    fn snapshot(&self) -> PrinterSnapshot {
        self.state.clone_snapshot()
    }

    fn dispatch(&mut self, _command: PrinterCommand) -> PrinterResult<()> {
        Err(PrinterError::PrinterUnavailable("Snapmaker U1 is disconnected".to_string()))
    }

    fn subscribe(&mut self, _observer: Weak<RefCell<dyn PrinterObserver>>) {}

    fn start(&mut self, framework: Rc<RefCell<Framework>>) {
        if self.task_started {
            return;
        }
        let spawner = framework.borrow().spawner;
        match spawner.spawn_heap(snapmaker_u1_comm_task(framework, self.config.clone(), self.printer_number)) {
            Ok(()) => self.task_started = true,
            Err(err) => warn!("[{}] Failed to start Snapmaker U1 communication task: {:?}", self.printer_number, err),
        }
    }
}

// #[embassy_executor::task]
async fn snapmaker_u1_comm_task(framework: Rc<RefCell<Framework>>, config: SnapmakerU1PrinterConfig, printer_number: usize) {
    Framework::wait_for_wifi(&framework).await;
    if let Some(ip) = config.ip {
        term_info!("[{}] Snapmaker U1: using configured IP {}", printer_number, ip);
    } else {
        discover_u1(&framework, &config, printer_number).await;
    }
    // Stage 3 will continue here with the connection and reconnect loop.
    pending::<()>().await;
}

async fn discover_u1(framework: &Rc<RefCell<Framework>>, config: &SnapmakerU1PrinterConfig, printer_number: usize) {
    let Some(serial) = config.serial.as_deref().map(str::trim).filter(|serial| !serial.is_empty()) else {
        return;
    };
    if let Err(err) = framework.borrow_mut().start_mdns_service() {
        term_info!("[{}] Snapmaker U1: could not start mDNS: {:?}", printer_number, err);
        return;
    }
    let discovery = framework.borrow().mdns_discovery.clone();
    let subscription = match discovery.subscribe(Some("_snapmaker._tcp.local")) {
        Ok(subscription) => subscription,
        Err(err) => {
            term_info!("[{}] Snapmaker U1: discovery unavailable: {}", printer_number, err);
            return;
        }
    };
    term_info!("[{}] Snapmaker U1: discovering printer", printer_number);
    let mut reported_serials: Vec<String> = Vec::new();
    loop {
        let service = subscription.next().await;
        if !service
            .txt_value("machine_type")
            .is_some_and(|value| value.eq_ignore_ascii_case("Snapmaker U1"))
        {
            continue;
        }
        let Some(found_serial) = service.txt_value("sn") else {
            continue;
        };
        if !found_serial.eq_ignore_ascii_case(serial) {
            if !reported_serials.iter().any(|reported| reported.eq_ignore_ascii_case(found_serial)) {
                warn!("[{}] Found Snapmaker U1 with serial {}, but configured serial is {}", printer_number, found_serial, serial);
                reported_serials.push(found_serial.to_string());
            }
            continue;
        }
        if let Some(ip) = service.addresses.first() {
            let name = service.txt_value("device_name").unwrap_or("Snapmaker U1");
            term_info!("[{}] Discovered Snapmaker U1 {} at {}", printer_number, name, ip);
            info!("[{}] Snapmaker U1 serial {} discovered at {}", printer_number, serial, ip);
            break;
        }
    }
}
