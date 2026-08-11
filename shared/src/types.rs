use serde::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AppOtaTrain {
    #[default]
    Stable,
    Unstable,
    Debug,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Eq, PartialEq)]
pub enum WifiSignal {
    Unknown,
    Disconnected,
    ConnectedNoSignal,
    Connected { rssi_dbm: i32, bars: u8 },
}
