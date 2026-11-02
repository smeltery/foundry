//! Local newline-delimited MCP transport. No hosted service or OAuth is provided.
mod tools;
mod transport;

use serde_json::{Value, json};
use std::borrow::Cow;
pub use tools::list_tools;
pub use transport::{serve, serve_stdio};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    pub code: String,
    pub message: String,
}

impl McpError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn unsupported() -> Self {
        Self::new(
            "unsupported",
            "Operation is not implemented by this backend",
        )
    }
}

/// Called only after tool name and arguments have passed the advertised schema.
/// Implementations own authorization, persistence, and session lifecycle rules.
pub trait Backend {
    fn call(&mut self, name: &str, args: Value) -> Result<Value, McpError>;
}

#[derive(Default)]
pub struct Server {
    initialized: bool,
    ready: bool,
}

pub(crate) fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

impl Server {
    /// Handle one UTF-8 JSON message. Notifications never invoke tools or reply.
    pub fn handle(&mut self, frame: &[u8], backend: &mut impl Backend) -> Option<Value> {
        if frame.len() > MAX_FRAME_BYTES {
            return Some(error(Value::Null, -32600, "Frame exceeds size limit"));
        }
        let request: Value = match serde_json::from_slice(frame) {
            Ok(request) => request,
            Err(_) => return Some(error(Value::Null, -32700, "Parse error")),
        };
        let Some(object) = request.as_object() else {
            return Some(error(Value::Null, -32600, "Expected a request object"));
        };
        let id = object.get("id");
        if id.is_some_and(|id| !(id.is_string() || id.is_i64() || id.is_u64())) {
            return Some(error(Value::Null, -32600, "Invalid request ID"));
        }
        let reply_id = id.cloned().unwrap_or(Value::Null);
        let Some(method) = request["method"].as_str() else {
            return Some(error(reply_id, -32600, "Missing method"));
        };
        if request["jsonrpc"] != "2.0"
            || object
                .keys()
                .any(|key| !["jsonrpc", "id", "method", "params"].contains(&key.as_str()))
        {
            return Some(error(reply_id, -32600, "Invalid JSON-RPC envelope"));
        }
        let empty = json!({});
        let params = object.get("params").unwrap_or(&empty);
        let params = match without_metadata(params, id.is_some()) {
            Ok(params) => params,
            Err(message) => return id.map(|_| error(reply_id, -32602, message)),
        };
        let params = params.as_ref();
        if id.is_none() {
            if method == "notifications/initialized"
                && self.initialized
                && params.as_object().is_some_and(|params| params.is_empty())
            {
                self.ready = true;
            }
            return None;
        }
        let result = match method {
            "initialize" => self.initialize(params),
            "ping" if params.as_object().unwrap().is_empty() => Ok(json!({})),
            "ping" => Err((-32602, "Ping takes no parameters")),
            "tools/list" | "tools/call" if !self.ready => {
                Err((-32002, "Initialization handshake required"))
            }
            "tools/list" if params.as_object().unwrap().is_empty() => {
                Ok(json!({"tools":list_tools()}))
            }
            "tools/list" => Err((-32602, "Pagination and list parameters are unsupported")),
            "tools/call" => call_tool(params, backend),
            _ => Err((-32601, "Method not found")),
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":reply_id,"result":result}),
            Err((code, message)) => error(reply_id, code, message),
        })
    }

    fn initialize(&mut self, params: &Value) -> Result<Value, (i64, &'static str)> {
        if self.initialized {
            return Err((-32600, "Already initialized"));
        }
        let requested = params["protocolVersion"]
            .as_str()
            .ok_or((-32602, "Protocol version must be a string"))?;
        let selected = match requested {
            "2025-06-18" | PROTOCOL_VERSION => requested,
            _ => PROTOCOL_VERSION,
        };
        if !params["capabilities"].is_object()
            || !params["clientInfo"]["name"].is_string()
            || !params["clientInfo"]["version"].is_string()
        {
            return Err((-32602, "Invalid initialization parameters"));
        }
        self.initialized = true;
        Ok(
            json!({"protocolVersion":selected,"capabilities":{"tools":{}},
            "serverInfo":{"name":"foundry-mcp","version":env!("CARGO_PKG_VERSION")}}),
        )
    }
}

/// Protocol metadata is not a backend argument. Progress is optional: accepting a
/// token does not promise notifications or change the tools capability.
fn without_metadata(params: &Value, request: bool) -> Result<Cow<'_, Value>, &'static str> {
    let object = params.as_object().ok_or("Params must be an object")?;
    let Some(metadata) = object.get("_meta") else {
        return Ok(Cow::Borrowed(params));
    };
    let metadata = metadata.as_object().ok_or("_meta must be an object")?;
    if request
        && metadata
            .get("progressToken")
            .is_some_and(|token| !(token.is_string() || token.is_i64() || token.is_u64()))
    {
        return Err("progressToken must be a string or integer");
    }
    // Unknown metadata values are intentionally opaque, including reserved keys.
    let mut parameters = object.clone();
    parameters.remove("_meta");
    Ok(Cow::Owned(Value::Object(parameters)))
}

fn call_tool(params: &Value, backend: &mut impl Backend) -> Result<Value, (i64, &'static str)> {
    if params
        .as_object()
        .unwrap()
        .keys()
        .any(|key| !["name", "arguments"].contains(&key.as_str()))
    {
        return Err((-32602, "Unknown tool-call parameter"));
    }
    let name = params["name"]
        .as_str()
        .ok_or((-32602, "Tool name is required"))?;
    let empty = json!({});
    let args = params.get("arguments").unwrap_or(&empty);
    tools::validate_arguments(name, args).map_err(|message| (-32602, message))?;
    Ok(match backend.call(name, args.clone()) {
        Ok(value) => json!({"content":[{"type":"text","text":value.to_string()}],"isError":false}),
        Err(failure) => json!({"content":[{"type":"text","text":
            json!({"code":failure.code,"message":failure.message}).to_string()}],"isError":true}),
    })
}
