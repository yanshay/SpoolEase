use serde::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum AppOtaTrain {
    #[default]
    Stable,
    Unstable,
    Debug,
}
