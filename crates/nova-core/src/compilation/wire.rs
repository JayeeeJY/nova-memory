//! Reject duplicate keys, including opaque semantic JSON, before typed decoding.
//! Otherwise a map could silently lose one of two values during a round trip.
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeOwned, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use std::fmt;

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8], reason: &'static str) -> crate::Result<T> {
    if bytes.len() > super::MAX_DOCUMENT_BYTES {
        return Err(super::invalid("document_limit"));
    }
    let value: UniqueJson = serde_json::from_slice(bytes).map_err(|_| super::invalid(reason))?;
    serde_json::from_value(value.0).map_err(|_| super::invalid(reason))
}

struct UniqueJson(Value);
struct UniqueVisitor;
impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor)
    }
}
impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueJson;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("JSON with unique object keys")
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJson(value.into()))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueJson(value.into()))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueJson(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(value)
            .map(|n| UniqueJson(Value::Number(n)))
            .ok_or_else(|| E::custom("invalid number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueJson(value.into()))
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueJson(value.into()))
    }
    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson(Value::Null))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = access.next_element::<UniqueJson>()? {
            if values.len() >= 1024 {
                return Err(de::Error::custom("array limit"));
            }
            values.push(value.0);
        }
        Ok(UniqueJson(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some((key, value)) = access.next_entry::<String, UniqueJson>()? {
            if values.len() >= 1024 || values.insert(key, value.0).is_some() {
                return Err(de::Error::custom("duplicate key or object limit"));
            }
        }
        Ok(UniqueJson(Value::Object(values)))
    }
}
