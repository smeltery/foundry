use crate::McpError;
use serde_json::Value;

pub(crate) fn rpc_result(value: Value, id: u64) -> Result<Value, McpError> {
    let object = value.as_object().ok_or(McpError::Protocol)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_u64) != Some(id)
        || object.contains_key("method")
        || object.contains_key("result") == object.contains_key("error")
    {
        return Err(McpError::Protocol);
    }
    if let Some(error) = object.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or(McpError::Protocol)?;
        return Err(McpError::RemoteRpc(code));
    }
    object
        .get("result")
        .filter(|result| result.is_object())
        .cloned()
        .ok_or(McpError::Protocol)
}

/// Bounded by the caller's total byte/time limits. No reconnection or replay.
#[derive(Default)]
pub(crate) struct EventDecoder {
    pending: Vec<u8>,
    data: String,
    first: bool,
}
impl EventDecoder {
    pub fn push(&mut self, bytes: &[u8], id: u64) -> Result<Option<Value>, McpError> {
        self.pending.extend_from_slice(bytes);
        let mut cursor = 0;
        while let Some(relative) = self.pending[cursor..]
            .iter()
            .position(|c| *c == b'\n' || *c == b'\r')
        {
            let end = cursor + relative;
            if self.pending[end] == b'\r' && end + 1 == self.pending.len() {
                break;
            }
            let width = if self.pending[end..].starts_with(b"\r\n") {
                2
            } else {
                1
            };
            let line = std::str::from_utf8(&self.pending[cursor..end])
                .map_err(|_| McpError::Protocol)?
                .to_owned();
            cursor = end + width;
            let line = if !self.first {
                self.first = true;
                line.trim_start_matches('\u{feff}')
            } else {
                &line
            };
            if line.is_empty() {
                if self.data.is_empty() {
                    continue;
                }
                let value: Value =
                    serde_json::from_str(&self.data).map_err(|_| McpError::Protocol)?;
                self.data.clear();
                if value.get("method").is_some() {
                    if value.get("id").is_some() {
                        return Err(McpError::UnsupportedServerMessage);
                    }
                    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                        || !value.get("method").is_some_and(Value::is_string)
                    {
                        return Err(McpError::Protocol);
                    }
                    continue;
                }
                return rpc_result(value, id).map(Some);
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            if field == "data" {
                self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
                self.data.push('\n');
            }
        }
        self.pending.drain(..cursor);
        Ok(None)
    }

    pub fn finish(&mut self, id: u64) -> Result<Option<Value>, McpError> {
        if self.pending.last() == Some(&b'\r') {
            self.push(b"\n", id)
        } else {
            Ok(None)
        }
    }
}
