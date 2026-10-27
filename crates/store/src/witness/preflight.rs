//! Bounded lexical pass before serde_json allocates a document tree.
use super::{
    MAX_WITNESS_BYTES, MAX_WITNESS_DEPTH, MAX_WITNESS_LOGICAL_RECORDS, MAX_WITNESS_PREFIX_RECORDS,
};
use crate::{Result, checkpoint_codec::invalid};

pub(super) fn check(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_WITNESS_BYTES {
        return Err(invalid("witness encoded byte limit"));
    }
    let mut parser = Parser { bytes, position: 0 };
    parser.value(1, MAX_WITNESS_PREFIX_RECORDS)?;
    if parser.position != bytes.len() {
        return Err(invalid("witness trailing bytes"));
    }
    Ok(())
}
struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }
    fn take(&mut self, byte: u8) -> Result<()> {
        if self.peek() != Some(byte) {
            return Err(invalid("noncanonical witness syntax"));
        }
        self.position += 1;
        Ok(())
    }
    fn string(&mut self) -> Result<String> {
        let start = self.position;
        self.take(b'"')?;
        loop {
            if self.position - start > 768 {
                return Err(invalid("witness encoded identifier limit"));
            }
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    break;
                }
                Some(b'\\') => {
                    self.position += 1;
                    if self.peek().is_none() {
                        return Err(invalid("truncated witness string"));
                    }
                    self.position += 1;
                }
                Some(_) => self.position += 1,
                None => return Err(invalid("truncated witness string")),
            }
        }
        let text: String = serde_json::from_slice(&self.bytes[start..self.position])?;
        crate::checkpoint_codec::identifier(&text)?;
        Ok(text)
    }
    fn value(&mut self, depth: usize, array_limit: usize) -> Result<()> {
        if depth > MAX_WITNESS_DEPTH {
            return Err(invalid("witness depth limit"));
        }
        match self.peek() {
            Some(b'{') => {
                self.position += 1;
                let mut previous: Option<String> = None;
                let mut count = 0;
                if self.peek() == Some(b'}') {
                    self.position += 1;
                    return Ok(());
                }
                loop {
                    if count == 32 {
                        return Err(invalid("witness object field limit"));
                    }
                    let key = self.string()?;
                    if previous.as_ref().is_some_and(|old| old >= &key) {
                        return Err(invalid("duplicate or unordered witness key"));
                    }
                    let limit = if key == "records" {
                        MAX_WITNESS_LOGICAL_RECORDS
                    } else {
                        MAX_WITNESS_PREFIX_RECORDS
                    };
                    self.take(b':')?;
                    self.value(depth + 1, limit)?;
                    previous = Some(key);
                    count += 1;
                    if self.peek() == Some(b'}') {
                        self.position += 1;
                        break;
                    }
                    self.take(b',')?;
                }
            }
            Some(b'[') => {
                self.position += 1;
                let mut count = 0;
                if self.peek() == Some(b']') {
                    self.position += 1;
                    return Ok(());
                }
                loop {
                    if count == array_limit {
                        return Err(invalid("witness array count limit"));
                    }
                    self.value(depth + 1, MAX_WITNESS_PREFIX_RECORDS)?;
                    count += 1;
                    if self.peek() == Some(b']') {
                        self.position += 1;
                        break;
                    }
                    self.take(b',')?;
                }
            }
            Some(b'"') => {
                self.string()?;
            }
            Some(b'n') => {
                for byte in b"null" {
                    self.take(*byte)?;
                }
            }
            Some(b'0'..=b'9') => {
                let start = self.position;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.position += 1;
                }
                let number = &self.bytes[start..self.position];
                if number.len() > 19 || (number.len() > 1 && number[0] == b'0') {
                    return Err(invalid("witness integer encoding"));
                }
                let value = std::str::from_utf8(number)
                    .unwrap()
                    .parse::<u64>()
                    .map_err(|_| invalid("witness integer range"))?;
                if value > i64::MAX as u64 {
                    return Err(invalid("witness integer range"));
                }
            }
            _ => return Err(invalid("noncanonical witness token")),
        }
        Ok(())
    }
}
