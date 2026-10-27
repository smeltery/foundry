//! Internal typed codec: public witness types implement neither serde trait.
use crate::{Result, checkpoint_codec::invalid};
use serde_json::{Map, Value};
use std::io::Write;

pub(super) trait Wire: Sized {
    fn write(&self, output: &mut impl Write) -> Result<()>;
    fn read(value: Value) -> Result<Self>;
}
pub(super) fn raw(output: &mut impl Write, bytes: &[u8]) -> Result<()> {
    output
        .write_all(bytes)
        .map_err(|_| invalid("witness encoded byte limit"))
}
impl Wire for String {
    fn write(&self, output: &mut impl Write) -> Result<()> {
        crate::checkpoint_codec::identifier(self)?;
        serde_json::to_writer(output, self).map_err(Into::into)
    }
    fn read(value: Value) -> Result<Self> {
        let Value::String(text) = value else {
            return Err(invalid("witness string required"));
        };
        crate::checkpoint_codec::identifier(&text)?;
        Ok(text)
    }
}
impl Wire for u64 {
    fn write(&self, output: &mut impl Write) -> Result<()> {
        if *self > i64::MAX as u64 {
            return Err(invalid("witness integer range"));
        }
        write!(output, "{self}").map_err(|_| invalid("witness encoded byte limit"))
    }
    fn read(value: Value) -> Result<Self> {
        value
            .as_u64()
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or_else(|| invalid("witness unsigned integer required"))
    }
}
impl Wire for [u8; 32] {
    fn write(&self, output: &mut impl Write) -> Result<()> {
        const HEX: &[u8] = b"0123456789abcdef";
        let mut bytes = [b'"'; 66];
        for (index, byte) in self.iter().enumerate() {
            bytes[1 + index * 2] = HEX[(byte >> 4) as usize];
            bytes[2 + index * 2] = HEX[(byte & 15) as usize];
        }
        raw(output, &bytes)
    }
    fn read(value: Value) -> Result<Self> {
        let text = String::read(value)?;
        if text.len() != 64
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("witness lowercase digest required"));
        }
        let mut digest = [0; 32];
        for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            let digit = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b - b'a' + 10
                }
            };
            digest[index] = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Ok(digest)
    }
}
impl<T: Wire> Wire for Option<T> {
    fn write(&self, output: &mut impl Write) -> Result<()> {
        match self {
            Some(value) => value.write(output),
            None => raw(output, b"null"),
        }
    }
    fn read(value: Value) -> Result<Self> {
        if value.is_null() {
            Ok(None)
        } else {
            T::read(value).map(Some)
        }
    }
}
impl<T: Wire> Wire for Vec<T> {
    fn write(&self, output: &mut impl Write) -> Result<()> {
        raw(output, b"[")?;
        for (index, value) in self.iter().enumerate() {
            if index != 0 {
                raw(output, b",")?;
            }
            value.write(output)?;
        }
        raw(output, b"]")
    }
    fn read(value: Value) -> Result<Self> {
        let Value::Array(values) = value else {
            return Err(invalid("witness array required"));
        };
        values.into_iter().map(T::read).collect()
    }
}
pub(super) struct Fields(Map<String, Value>);
impl Fields {
    pub fn new(value: Value) -> Result<Self> {
        match value {
            Value::Object(map) => Ok(Self(map)),
            _ => Err(invalid("witness object required")),
        }
    }
    pub fn take<T: Wire>(&mut self, name: &str) -> Result<T> {
        T::read(
            self.0
                .remove(name)
                .ok_or_else(|| invalid("missing witness field"))?,
        )
    }
    pub fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid("unknown witness field"))
        }
    }
}
macro_rules! object {
    ($type:ty { $($field:ident),+ $(,)? }) => {
        impl Wire for $type {
            fn write(&self, output: &mut impl std::io::Write) -> crate::Result<()> {
                raw(output,b"{")?;
                let mut separator = b"".as_slice();
                $(raw(output,separator)?;
                  raw(output,concat!("\"",stringify!($field),"\":").as_bytes())?;
                  self.$field.write(output)?;
                  separator=b",";)+
                let _ = separator;
                raw(output,b"}")
            }
            fn read(value: serde_json::Value) -> crate::Result<Self> {
                let mut fields = Fields::new(value)?;
                let result = Self { $($field: fields.take(stringify!($field))?),+ };
                fields.finish()?;
                Ok(result)
            }
        }
    }
}
pub(super) use object;
macro_rules! tagged {
    ($type:ty {$($variant:ident => $tag:literal),+ $(,)?}) => {
        impl Wire for $type {
            fn write(&self, output: &mut impl std::io::Write) -> crate::Result<()> {
                match self {$(Self::$variant(value) => {
                    raw(output,concat!("{\"",$tag,"\":").as_bytes())?;
                    value.write(output)?;
                    raw(output,b"}")
                }),+}
            }
            fn read(value: serde_json::Value) -> crate::Result<Self> {
                let serde_json::Value::Object(map) = value else {return Err(invalid("witness tagged object required"));};
                if map.len()!=1 {return Err(invalid("witness single variant required"));}
                let (tag,value)=map.into_iter().next().unwrap();
                match tag.as_str() {$($tag => Ok(Self::$variant(Wire::read(value)?)),)+_=>Err(invalid("unknown witness variant"))}
            }
        }
    }
}
pub(super) use tagged;
macro_rules! choice {
    ($type:ty {$($variant:ident => $tag:literal),+ $(,)?}) => {
        impl Wire for $type {
            fn write(&self, output: &mut impl std::io::Write) -> crate::Result<()> {
                raw(output,match self {$(Self::$variant=>concat!("\"",$tag,"\"").as_bytes(),)+})
            }
            fn read(value: serde_json::Value) -> crate::Result<Self> {
                match value.as_str() {$(Some($tag)=>Ok(Self::$variant),)+_=>Err(invalid("unknown witness choice"))}
            }
        }
    }
}
pub(super) use choice;
