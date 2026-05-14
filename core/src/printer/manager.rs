use alloc::{
    boxed::Box,
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;

use crate::app_config::FakePrinterConfig;
use crate::bambu::BambuPrinter;

use super::{
    PrinterCommand, PrinterDriver, PrinterError, PrinterId, PrinterResult, PrinterSnapshot, bambu_adapter::BambuPrinterDriver,
    fake_driver::FakePrinterDriver,
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
        self.add_driver(Box::new(FakePrinterDriver::new(name, config)));
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

    pub fn id_at(&self, index: usize) -> Option<PrinterId> {
        self.printers.get(index).map(|printer| printer.id().clone())
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

    pub fn dispatch_bambu_printer(&mut self, printer: &mut BambuPrinter, command: PrinterCommand) -> PrinterResult<()> {
        BambuPrinterDriver::dispatch_to_printer(printer, command)
    }
}
