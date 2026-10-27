use crate::{ConnectorError, validation};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Catalog {
    pub schema_version: u32,
    pub connectors: Vec<Connector>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Connector {
    pub id: String,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub fixture: bool,
    pub homepage: Option<String>,
    pub documentation: Option<String>,
    pub icon: Option<Icon>,
    pub servers: Vec<Server>,
    pub skills: Option<Vec<Skill>>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Icon {
    Local { path: String },
    Remote { url: String },
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Skill {
    pub id: String,
    pub url: String,
    pub revision: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub id: String,
    pub transport: Transport,
    pub endpoint: Endpoint,
    pub auth: Auth,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum Transport {
    #[serde(rename = "streamable-http")]
    StreamableHttp,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Endpoint {
    Remote { url: String },
    Adapter { id: String, state: AdapterState },
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterState {
    Unavailable,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Auth {
    None {},
    Oauth {},
    ApiKey { placeholder: Placeholder },
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Placeholder {
    Credential,
}

impl Catalog {
    /// Reject duplicate JSON keys before deserializing into typed objects.
    pub fn parse(bytes: &[u8]) -> Result<Self, ConnectorError> {
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(ConnectorError::InvalidCatalog);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let value =
            UniqueValue::deserialize(&mut deserializer).map_err(|_| ConnectorError::InvalidJson)?;
        deserializer
            .end()
            .map_err(|_| ConnectorError::InvalidJson)?;
        let catalog: Self =
            serde_json::from_value(value.0).map_err(|_| ConnectorError::InvalidCatalog)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), ConnectorError> {
        validation::catalog(self)
    }
}

// JSON maps otherwise silently collapse duplicate header keys.
struct UniqueValue(serde_json::Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, UniqueValue>()? {
                    if values.insert(key, value.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                }
                Ok(UniqueValue(serde_json::Value::Object(values)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<UniqueValue>()? {
                    values.push(value.0);
                }
                Ok(UniqueValue(serde_json::Value::Array(values)))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(value.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(UniqueValue(serde_json::Value::Number(
                    serde_json::Number::from_f64(value)
                        .ok_or_else(|| serde::de::Error::custom("invalid number"))?,
                )))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Err(serde::de::Error::custom(
                    "null is not in the catalog schema",
                ))
            }
        }
        d.deserialize_any(Visitor)
    }
}

macro_rules! redacted_debug {
    ($($name:ty),+) => {$(impl fmt::Debug for $name {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(concat!(stringify!($name), " { values: [redacted] }"))
        }
    })+};
}
redacted_debug!(Catalog, Connector, Server, Endpoint, Icon, Skill);
