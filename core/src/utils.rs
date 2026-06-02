use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use serde::{Deserialize, Deserializer, Serializer};
use sha2::{Digest, Sha256};

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn canonical_string_list_bytes<I, S>(values: I) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut values: Vec<String> = values.into_iter().map(|value| value.as_ref().to_string()).collect();
    values.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

    let mut output = Vec::new();
    for value in values {
        let value_bytes = value.as_bytes();
        output.extend_from_slice(value_bytes.len().to_string().as_bytes());
        output.push(b':');
        output.extend_from_slice(value_bytes);
        output.push(b';');
    }
    output
}

pub fn hash_string_list<I, S>(values: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    sha256_hex(&canonical_string_list_bytes(values))
}

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

pub fn serialize_line_safe_string<S>(value: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&encode_line_safe_string(value))
}

pub fn deserialize_line_safe_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let serialized: String = Deserialize::deserialize(deserializer)?;
    Ok(decode_line_safe_string(&serialized))
}

fn encode_line_safe_string(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            _ => encoded.push(ch),
        }
    }
    encoded
}

fn decode_line_safe_string(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => decoded.push('\n'),
                Some('r') => decoded.push('\r'),
                Some('\\') => decoded.push('\\'),
                Some(other) => {
                    decoded.push('\\');
                    decoded.push(other);
                }
                None => decoded.push('\\'),
            }
        } else {
            decoded.push(ch);
        }
    }
    decoded
}
