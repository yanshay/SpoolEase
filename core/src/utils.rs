use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize_string_array<S>(values: &[String], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&values.join(";"))
}

pub fn deserialize_string_array<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let serialized: String = Deserialize::deserialize(deserializer)?;
    if serialized.is_empty() {
        return Ok(Vec::new());
    }

    Ok(serialized
        .split(';')
        .filter(|tag_id| !tag_id.is_empty())
        .map(ToString::to_string)
        .collect())
}
