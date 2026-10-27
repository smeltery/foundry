use super::{Result, ToolProcessError as Error};
use foundry_tools::host::{
    self, Effect, ErrorCode, Grant, Input, Outcome, Reply, Request, WriteMode,
};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use std::{
    fmt,
    io::{self, Read, Write},
};

struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > host::MAX_REQUEST_BYTES.saturating_sub(self.0.len()) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn prepare(request: &Request) -> Result<Vec<u8>> {
    let mut encoded = Bounded(Vec::new());
    serde_json::to_writer(&mut encoded, request).map_err(|_| Error::Invalid)?;
    let mut frame = (encoded.0.len() as u32).to_be_bytes().to_vec();
    frame.extend(encoded.0);
    host::read_request(&mut io::Cursor::new(&frame)).map_err(|_| Error::Invalid)?;
    let grant = match (&*request.tool, &request.input) {
        ("read", Input::Path(_)) => Grant::Read,
        ("list", Input::Path(_)) => Grant::List,
        ("write", Input::Write(args)) => match args.mode {
            WriteMode::Create => Grant::Create,
            WriteMode::Overwrite => Grant::Overwrite,
        },
        _ => return Err(Error::Invalid),
    };
    if !request.grants.contains(&grant) {
        return Err(Error::Denied);
    }
    Ok(frame)
}
pub(super) fn read_reply(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    reader
        .read_exact(&mut length)
        .map_err(|_| Error::Protocol)?;
    let size = u32::from_be_bytes(length) as usize;
    if size == 0 || size > host::MAX_REPLY_BYTES {
        return Err(Error::Protocol);
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).map_err(|_| Error::Protocol)?;
    let mut extra = [0];
    if reader.read(&mut extra).map_err(|_| Error::Io)? != 0 {
        return Err(Error::Protocol);
    }
    Ok(bytes)
}
pub(super) fn validate(bytes: &[u8], request: &Request) -> Result<Reply> {
    // Deserialize recursively before Value can collapse duplicate keys.
    let Unique(value) = serde_json::from_slice(bytes).map_err(|_| Error::Protocol)?;
    let reply: Reply = serde_json::from_value(value).map_err(|_| Error::Protocol)?;
    if reply.version != host::VERSION
        || reply.call_id != request.call_id
        || reply.attempt_id != request.attempt_id
    {
        return Err(Error::Protocol);
    }
    if let Outcome::Error {
        code,
        effect: Effect::None,
    } = &reply.result
        && request.tool == "write"
        && !matches!(code, ErrorCode::Denied | ErrorCode::UnknownTool)
    {
        return Err(Error::Protocol);
    }
    if let Outcome::Ok { output } = &reply.result {
        if serde_json::to_vec(output)
            .map_err(|_| Error::Protocol)?
            .len()
            > request.limits.output_bytes
        {
            return Err(Error::Protocol);
        }
        let object = output
            .as_object()
            .filter(|map| map.len() == 1)
            .ok_or(Error::Protocol)?;
        match (&*request.tool, &request.input) {
            ("read", Input::Path(_)) => {
                let content = object
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or(Error::Protocol)?;
                if content.len() > request.limits.read_bytes {
                    return Err(Error::Protocol);
                }
            }
            ("write", Input::Write(input)) => {
                if input.content.len() > request.limits.write_bytes
                    || object.get("bytes_written").and_then(Value::as_u64)
                        != Some(input.content.len() as u64)
                {
                    return Err(Error::Protocol);
                }
            }
            ("list", Input::Path(_)) => {
                let entries = object
                    .get("entries")
                    .and_then(Value::as_array)
                    .ok_or(Error::Protocol)?;
                if entries.len() > request.limits.entries {
                    return Err(Error::Protocol);
                }
                let mut previous = None;
                for entry in entries {
                    let entry = entry
                        .as_object()
                        .filter(|map| map.len() == 2)
                        .ok_or(Error::Protocol)?;
                    let name = entry
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(Error::Protocol)?;
                    let kind = entry
                        .get("kind")
                        .and_then(Value::as_str)
                        .ok_or(Error::Protocol)?;
                    if name.is_empty()
                        || name.contains(['\0', '/'])
                        || matches!(name, "." | "..")
                        || previous.is_some_and(|old| old >= name)
                        || !matches!(kind, "file" | "directory" | "symlink" | "other")
                    {
                        return Err(Error::Protocol);
                    }
                    previous = Some(name);
                }
            }
            _ => return Err(Error::Protocol),
        }
    }
    Ok(reply)
}

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Checked;
        impl<'de> Visitor<'de> for Checked {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unique-key JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate key"));
                    }
                    let Unique(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(Checked)
    }
}
