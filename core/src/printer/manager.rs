use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;
use framework::framework::Framework;

use crate::app_config::FakePrinterConfig;
use crate::bambu::BambuPrinter;
use crate::store::Store;

use super::{
    DriverSpecificQuery, DriverSpecificQueryResult, PRINTER_STATE_FILE_VERSION, PrinterCapabilities, PrinterCommand, PrinterDriver,
    PrinterError, PrinterId, PrinterObserver, PrinterPersistentStatePayload, PrinterResult, PrinterRuntimePersistenceFuture,
    PrinterRuntimePersistenceRequestKind, PrinterSnapshot, PrinterStateFile, SlotId, bambu_adapter::BambuPrinterDriver,
    fake_driver::FakePrinterDriver, sanitize_loaded_snapshot,
};

#[derive(Default)]
pub struct PrinterManager {
    printers: Vec<Box<dyn PrinterDriver>>,
    selected_index: Option<usize>,
}

impl PrinterManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_bambu_printer(&mut self, printer: Rc<RefCell<BambuPrinter>>) {
        self.add_driver(Box::new(BambuPrinterDriver::new(printer)));
    }

    pub fn add_fake_printer(&mut self, name: Option<String>, config: &FakePrinterConfig) {
        let printer_number = self.printers.len() + 1;
        self.add_driver(Box::new(FakePrinterDriver::new(name, config, printer_number)));
    }

    fn add_driver(&mut self, driver: Box<dyn PrinterDriver>) {
        self.printers.push(driver);
        if self.selected_index.is_none() {
            self.selected_index = Some(0);
        }
    }

    pub fn len(&self) -> usize {
        self.printers.len()
    }

    pub fn set_selected_index(&mut self, index: usize) -> PrinterResult<()> {
        if index < self.printers.len() {
            self.selected_index = Some(index);
            Ok(())
        } else {
            Err(PrinterError::PrinterUnavailable(format!("printer index {index}")))
        }
    }

    pub fn snapshot_at(&self, index: usize) -> Option<PrinterSnapshot> {
        self.printers.get(index).map(|printer| printer.snapshot())
    }

    pub fn capabilities_at(&self, index: usize) -> Option<PrinterCapabilities> {
        self.printers.get(index).map(|printer| printer.capabilities())
    }

    pub fn query_driver_specific_at(&self, index: usize, query: DriverSpecificQuery) -> PrinterResult<DriverSpecificQueryResult> {
        self.printers
            .get(index)
            .ok_or_else(|| PrinterError::PrinterUnavailable(format!("printer index {index}")))?
            .query_driver_specific(query)
    }

    pub fn id_at(&self, index: usize) -> Option<PrinterId> {
        self.printers.get(index).map(|printer| printer.id().clone())
    }

    pub fn printer_number_at(&self, index: usize) -> Option<usize> {
        self.printers.get(index).map(|_| index + 1)
    }

    pub fn printer_number_by_id(&self, printer_id: &PrinterId) -> Option<usize> {
        self.index_by_id(printer_id).map(|index| index + 1)
    }

    pub fn index_by_id(&self, printer_id: &PrinterId) -> Option<usize> {
        self.printers.iter().position(|printer| printer.id() == printer_id)
    }

    pub fn dispatch_selected(&mut self, command: PrinterCommand) -> PrinterResult<()> {
        let selected_index = self
            .selected_index
            .ok_or_else(|| PrinterError::PrinterUnavailable("no selected printer".to_string()))?;
        self.dispatch_at(selected_index, command)
    }

    pub fn dispatch_at(&mut self, index: usize, command: PrinterCommand) -> PrinterResult<()> {
        self.printers
            .get_mut(index)
            .ok_or_else(|| PrinterError::PrinterUnavailable(format!("printer index {index}")))?
            .dispatch(command)
    }

    pub fn dispatch_by_id(&mut self, printer_id: &PrinterId, command: PrinterCommand) -> PrinterResult<()> {
        self.printers
            .iter_mut()
            .find(|printer| printer.id() == printer_id)
            .ok_or_else(|| PrinterError::PrinterUnavailable(format!("printer id {}", printer_id.as_str())))?
            .dispatch(command)
    }

    pub fn acknowledge_slot_consumption_saved_by_id(
        &mut self,
        printer_id: &PrinterId,
        slot_id: &SlotId,
        consumed_since_load_saved_g: f32,
    ) -> PrinterResult<()> {
        self.printers
            .iter_mut()
            .find(|printer| printer.id() == printer_id)
            .ok_or_else(|| PrinterError::PrinterUnavailable(format!("printer id {}", printer_id.as_str())))?
            .acknowledge_slot_consumption_saved(slot_id, consumed_since_load_saved_g)
    }

    pub fn subscribe_at(&mut self, index: usize, observer: alloc::rc::Weak<RefCell<dyn PrinterObserver>>) -> PrinterResult<()> {
        self.printers
            .get_mut(index)
            .ok_or_else(|| PrinterError::PrinterUnavailable(format!("printer index {index}")))?
            .subscribe(observer);
        Ok(())
    }

    pub fn start_at(&mut self, index: usize, framework: Rc<RefCell<Framework>>) -> PrinterResult<()> {
        self.printers
            .get_mut(index)
            .ok_or_else(|| PrinterError::PrinterUnavailable(format!("printer index {index}")))?
            .start(framework);
        Ok(())
    }

    pub fn persistent_state_path_at(&self, index: usize) -> Option<String> {
        self.printers.get(index).and_then(|printer| printer.persistent_state_path())
    }

    pub fn persistent_state_paths(&self) -> Vec<(usize, PrinterId, String)> {
        self.printers
            .iter()
            .enumerate()
            .filter_map(|(index, printer)| printer.persistent_state_path().map(|path| (index, printer.id().clone(), path)))
            .collect()
    }

    pub fn load_persistent_state_at(&mut self, index: usize, state_json: &str, store: &Rc<Store>) -> Result<(), String> {
        let driver = self.printers.get_mut(index).ok_or_else(|| format!("printer index {index}"))?;
        let mut state = serde_json::from_str::<PrinterStateFile>(state_json).map_err(|err| format!("Failed to parse printer state: {err}"))?;
        if state.version != PRINTER_STATE_FILE_VERSION {
            return Err(format!("Unsupported printer state version {}", state.version));
        }
        if state.printer_id != *driver.id() {
            return Err(format!(
                "State file belongs to {}, not {}",
                state.printer_id.as_str(),
                driver.id().as_str()
            ));
        }
        if state.driver_kind != driver.kind() {
            return Err(format!("State file has unexpected driver kind {:?}", state.driver_kind));
        }
        if state.generic.id != state.printer_id {
            return Err(format!(
                "State file generic snapshot belongs to {}, not {}",
                state.generic.id.as_str(),
                state.printer_id.as_str()
            ));
        }
        if state.generic.kind != state.driver_kind {
            return Err(format!("State file generic snapshot has unexpected driver kind {:?}", state.generic.kind));
        }

        let removed_missing_spools = Self::clear_missing_spool_ids(&mut state.generic, store);
        driver.load_private_state(state.driver_private.take(), store)?;
        sanitize_loaded_snapshot(&mut state.generic);
        driver.adjust_loaded_snapshot(&mut state.generic);
        let snapshot_state = driver.snapshot_state();
        snapshot_state.replace_loaded_sanitized(state.generic);
        if removed_missing_spools {
            snapshot_state.mark_dirty();
        }
        Ok(())
    }

    pub fn prepare_persistent_state_store_at(&mut self, index: usize) -> Result<Option<PrinterPersistentStatePayload>, String> {
        let driver = self.printers.get_mut(index).ok_or_else(|| format!("printer index {index}"))?;
        let Some(path) = driver.persistent_state_path() else {
            return Ok(None);
        };
        let snapshot_state = driver.snapshot_state();
        let generic_dirty = snapshot_state.is_dirty();
        let private_dirty = driver.private_state_dirty();
        if !generic_dirty && !private_dirty {
            return Ok(None);
        }

        let driver_private = driver.prepare_private_state_store()?;
        if private_dirty && driver_private.is_none() {
            return Ok(None);
        }

        snapshot_state.begin_store();
        let state = PrinterStateFile {
            version: PRINTER_STATE_FILE_VERSION,
            printer_id: driver.id().clone(),
            driver_kind: driver.kind(),
            generic: snapshot_state.clone_snapshot(),
            driver_private,
        };
        let contents = match serde_json::to_string(&state) {
            Ok(contents) => contents,
            Err(err) => {
                snapshot_state.store_failed();
                driver.restore_private_state_after_failed_store();
                return Err(format!("Failed to serialize printer state: {err}"));
            }
        };

        Ok(Some(PrinterPersistentStatePayload { path, contents }))
    }

    pub fn persistent_state_store_succeeded_at(&mut self, index: usize) -> Result<(), String> {
        let driver = self.printers.get_mut(index).ok_or_else(|| format!("printer index {index}"))?;
        driver.snapshot_state().store_succeeded();
        driver.private_state_store_succeeded();
        Ok(())
    }

    pub fn restore_persistent_state_after_failed_store_at(&mut self, index: usize) -> Result<(), String> {
        let driver = self.printers.get_mut(index).ok_or_else(|| format!("printer index {index}"))?;
        driver.snapshot_state().store_failed();
        driver.restore_private_state_after_failed_store();
        Ok(())
    }

    pub fn prepare_runtime_state_restore_at(
        &mut self,
        index: usize,
        framework: Rc<RefCell<Framework>>,
    ) -> Result<Option<PrinterRuntimePersistenceFuture>, String> {
        let driver = self.printers.get_mut(index).ok_or_else(|| format!("printer index {index}"))?;
        Ok(driver.restore_runtime_state(framework))
    }

    pub fn prepare_runtime_persistence_request_by_id(
        &mut self,
        printer_id: &PrinterId,
        framework: Rc<RefCell<Framework>>,
        request: PrinterRuntimePersistenceRequestKind,
    ) -> Result<Option<PrinterRuntimePersistenceFuture>, String> {
        let driver = self
            .printers
            .iter_mut()
            .find(|printer| printer.id() == printer_id)
            .ok_or_else(|| format!("printer id {}", printer_id.as_str()))?;
        Ok(driver.handle_runtime_persistence_request(framework, request))
    }

    fn clear_missing_spool_ids(snapshot: &mut PrinterSnapshot, store: &Rc<Store>) -> bool {
        let mut changed = false;
        for slot in snapshot.slot_groups.iter_mut().flat_map(|group| group.slots.iter_mut()) {
            let Some(spool_id) = slot.spool_id.as_ref() else {
                continue;
            };
            if store.get_spool_by_id(spool_id).is_none() {
                slot.spool_id = None;
                changed = true;
            }
        }
        changed
    }
}
