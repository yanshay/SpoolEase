use core::str::FromStr;

use alloc::string::String;

use crate::{
    bambu::{PrinterConnectMode, PrinterModel},
    ssdp::SSDPInfo,
};
use embassy_net::Ipv4Address;

#[derive(Clone, Debug, Default)]
pub struct BambuSSDPInfo {
    pub serial: Option<String>,
    pub name: Option<String>,
    pub ip: Option<Ipv4Address>,
    pub _model: Option<PrinterModel>,
    pub _connect_mode: Option<PrinterConnectMode>,
}

impl TryFrom<SSDPInfo> for BambuSSDPInfo {
    type Error = &'static str;
    fn try_from(v: SSDPInfo) -> Result<Self, Self::Error> {
        if v.nt.contains("urn:bambulab-com:device:3dprinter") {
            Ok(Self {
                serial: Some(v.usn),
                name: v.custom.get("DevName.bambu.com:").cloned(),
                ip: embassy_net::Ipv4Address::from_str(&v.location).ok(),
                _model: v.custom.get("DevModel.bambu.com").map(|s| match s.as_str() {
                    "3DPrinter-X1" => PrinterModel::X1,
                    "3DPrinter-X1-Carbon" => PrinterModel::X1C,
                    "C11" => PrinterModel::P1P,
                    "C12" => PrinterModel::P1S,
                    "C13" => PrinterModel::X1E,
                    "N1" => PrinterModel::A1Mini,
                    "N2" => PrinterModel::A1,
                    "N7" => PrinterModel::P2S,
                    _ => PrinterModel::Unknown,
                }),

                _connect_mode: v.custom.get("DevModel.bambu.com").map(|s| match s.as_str() {
                    "lan" => PrinterConnectMode::Lan,
                    "cloud" => PrinterConnectMode::Cloud,
                    _ => PrinterConnectMode::Unknown,
                }),
            })
        } else {
            Err("Not a Bambulab Printer SSDP")
        }
    }
}

// PRINTER_USN = "YOUR_PRINTER_SN" # This is the serial number of the printer. https://wiki.bambulab.com/en/general/find-sn
// PRINTER_DEV_MODEL = "3DPrinter-X1-Carbon" # "3DPrinter-X1-Carbon", "3DPrinter-X1", "C11" (for P1P), "C12" (for P1S), "C13" (for X1E), "N1" (A1 mini), "N2S" (A1)
// PRINTER_DEV_NAME = "X1C-1" # The friendly name displayed in Bambu Studio / Orca Slicer. Set this to whatever you want.
// PRINTER_DEV_SIGNAL = "-44" # Fake wifi signal strength
// PRINTER_DEV_CONNECT = "lan" # printer is in lan only mode
// PRINTER_DEV_BIND = "free" # and is not bound to any cloud account
// PRINTER_IP = None # If you want to hardcode the printer IP, set it here. Otherwise, pass it as the first argument to the script.
// TARGET_PORT = 2021 # The port used for SSDP discovery
