use super::{Error, Result};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::{cell::Cell, fmt, io::Write};
const SAFE: u64 = 9_007_199_254_740_991;
struct Seed<'a>(usize, &'a Cell<usize>);
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        decoder: D,
    ) -> std::result::Result<Value, D::Error> {
        if self.0 > 32 {
            return Err(serde::de::Error::custom("profile host depth"));
        }
        decoder.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded profile host value")
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(value.into()))
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> std::result::Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<Value, E> {
        if value > SAFE {
            return Err(E::custom("profile host integer"));
        }
        Ok(Value::Number(Number::from(value)))
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> std::result::Result<Value, E> {
        if value.unsigned_abs() > SAFE {
            return Err(E::custom("profile host integer"));
        }
        Ok(Value::Number(Number::from(value)))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(Seed(self.0 + 1, self.1))? {
            if values.len() == 1024 {
                return Err(serde::de::Error::custom("profile host array"));
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.len() == 128 || values.contains_key(&key) {
                return Err(serde::de::Error::custom("profile host fields"));
            }
            values.insert(key, map.next_value_seed(Seed(self.0 + 1, self.1))?);
        }
        if values.contains_key("base64") && values.contains_key("sha256") {
            let bytes = values
                .get("bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| serde::de::Error::custom("profile host blob"))?;
            if bytes > 1048576 || bytes as usize > 4 * 1048576 - self.1.get() {
                return Err(serde::de::Error::custom("profile host blob budget"));
            }
            self.1.set(self.1.get() + bytes as usize);
        }
        Ok(Value::Object(values))
    }
}
pub(super) fn decode(bytes: &[u8], limit: usize) -> Result<Value> {
    if bytes.len() > limit {
        return Err(Error::Invalid);
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let blobs = Cell::new(0);
    let value = Seed(0, &blobs)
        .deserialize(&mut decoder)
        .map_err(|_| Error::Invalid)?;
    decoder.end().map_err(|_| Error::Invalid)?;
    Ok(value)
}

// Refuse excessive serialization before growing a request buffer past its bound.
pub(super) fn encode(value: &impl serde::Serialize, limit: usize) -> Result<Vec<u8>> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| Error::Invalid)?;
    Ok(writer.bytes)
}
