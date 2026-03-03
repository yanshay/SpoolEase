use alloc::{format, string::String};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use core::{fmt::Display, str::FromStr};
use embassy_sync::blocking_mutex::raw::RawMutex;
use framework::error;
use serde::{
    Deserialize, Deserializer, Serializer,
    de::{Error, Unexpected},
};

pub fn deserialize_optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: Display,
{
    let s: String = String::deserialize(deserializer)?;
    if s.is_empty() {
        Ok(None)
    } else {
        s.parse()
            .map(Some)
            .map_err(|e| serde::de::Error::custom(format!("Parse error: {}", e)))
    }
}

#[allow(dead_code)]
pub fn deserialize_optional_unit<'de, D>(deserializer: D) -> Result<Option<()>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = String::deserialize(deserializer)?;
    if s.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(()))
    }
}

pub fn serialize_optional_bool_yn<S>(value: &Option<bool>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(true) => serializer.serialize_str("y"),
        Some(false) => serializer.serialize_str("n"),
        None => serializer.serialize_str(""),
    }
}

pub fn deserialize_optional_bool_yn<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.as_str() {
        "" => Ok(None),
        "y" | "Y" => Ok(Some(true)),
        "n" | "N" => Ok(Some(false)),
        _ => Err(D::Error::invalid_value(
            Unexpected::Str(&s),
            &r#""y", "n", or empty"#,
        )),
    }
}

pub fn serialize_bool_yn<S>(value: &bool, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        true => serializer.serialize_str("y"),
        false => serializer.serialize_str("n"),
    }
}

pub fn deserialize_bool_yn_empty_n<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.as_str() {
        "" => Ok(false),
        "y" | "Y" => Ok(true),
        "n" | "N" => Ok(false),
        _ => Err(D::Error::invalid_value(
            Unexpected::Str(&s),
            &r#""y", "n", or empty"#,
        )),
    }
}

pub fn deserialize_bool_yn_empty_y<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.as_str() {
        "" => Ok(true),
        "y" | "Y" => Ok(true),
        "n" | "N" => Ok(false),
        _ => Err(D::Error::invalid_value(
            Unexpected::Str(&s),
            &r#""y", "n", or empty"#,
        )),
    }
}

pub fn serialize_optional_f32_base64<S>(
    value: &Option<f32>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(v) => {
            let bytes = v.to_le_bytes();
            let b64 = STANDARD_NO_PAD.encode(bytes);
            serializer.serialize_str(&b64)
        }
        None => serializer.serialize_str(""),
    }
}

pub fn deserialize_optional_f32_base64<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        return Ok(None);
    }
    let bytes = STANDARD_NO_PAD
        .decode(&s)
        .map_err(serde::de::Error::custom)?;
    if bytes.len() != 4 {
        return Err(serde::de::Error::custom("Invalid length for f32 base64"));
    }
    let mut arr = [0u8; 4];
    arr.copy_from_slice(&bytes);
    Ok(Some(f32::from_le_bytes(arr)))
}

pub fn serialize_f32_base64<S>(value: &f32, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        0.0 => serializer.serialize_str(""),
        _ => {
            let bytes = value.to_le_bytes();
            let b64 = STANDARD_NO_PAD.encode(bytes);
            serializer.serialize_str(&b64)
        }
    }
}

pub fn deserialize_f32_base64<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        return Ok(0.0);
    }
    let bytes = STANDARD_NO_PAD
        .decode(&s)
        .map_err(serde::de::Error::custom)?;
    if bytes.len() != 4 {
        return Err(serde::de::Error::custom("Invalid length for f32 base64"));
    }
    let mut arr = [0u8; 4];
    arr.copy_from_slice(&bytes);
    Ok(f32::from_le_bytes(arr))
}

pub fn channel_send<T: core::fmt::Debug, M: RawMutex, const N: usize>(
    ch: &embassy_sync::channel::Channel<M, T, N>,
    msg: T,
) {
    let t = core::any::type_name::<T>();
    if let Err(e) = ch.sender().try_send(msg) {
        error!("Error dispatching messge in {t} : {:?}", e);
    }
}
