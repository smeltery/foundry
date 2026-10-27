use crate::{
    HttpLimits, McpError, NetworkPolicy, PreparedRequest,
    http::{HttpTransport, within},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, fmt};
use tokio::time::Instant;

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    pub name: String,
    pub input_schema: Value,
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPage {
    pub tools: Vec<ToolDefinition>,
    pub next_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content: Vec<Value>,
    #[serde(default)]
    pub is_error: bool,
    pub structured_content: Option<Value>,
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}

/// Sequential MCP client; retries, redirects and session recovery are never automatic.
pub struct McpHttpClient {
    transport: HttpTransport,
    session: Option<String>,
    initialized: bool,
    tools: bool,
    next_id: u64,
}

impl McpHttpClient {
    pub fn new(
        request: PreparedRequest,
        policy: NetworkPolicy,
        limits: HttpLimits,
    ) -> Result<Self, McpError> {
        Ok(Self {
            transport: HttpTransport::new(request, policy, limits)?,
            session: None,
            initialized: false,
            tools: false,
            next_id: 1,
        })
    }

    pub async fn initialize(&mut self) -> Result<(), McpError> {
        if self.initialized {
            return Err(McpError::AlreadyInitialized);
        }
        let deadline = Instant::now() + self.transport.limits.timeout;
        let result = within(deadline, self.initialize_inner()).await;
        if result.is_err() {
            self.session = None;
            self.tools = false;
        }
        result
    }

    async fn initialize_inner(&mut self) -> Result<(), McpError> {
        let result = self
            .exchange(
                "initialize",
                json!({"protocolVersion":MCP_PROTOCOL_VERSION,
            "capabilities":{},"clientInfo":{"name":"f5y","version":env!("CARGO_PKG_VERSION")}}),
                false,
            )
            .await?;
        if result.get("protocolVersion").and_then(Value::as_str) != Some(MCP_PROTOCOL_VERSION) {
            return Err(McpError::UnsupportedVersion);
        }
        if !result.get("capabilities").is_some_and(Value::is_object)
            || !result.get("serverInfo").is_some_and(|info| {
                info.get("name").is_some_and(Value::is_string)
                    && info.get("version").is_some_and(Value::is_string)
            })
        {
            return Err(McpError::Protocol);
        }
        self.tools = result["capabilities"]
            .get("tools")
            .is_some_and(Value::is_object);
        let body =
            serde_json::to_vec(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .map_err(|_| McpError::Protocol)?;
        let mut reply = self
            .transport
            .post(body, self.session.as_deref(), true, true)
            .await?;
        self.check_session(reply.session.as_deref())?;
        self.transport.bounded_body(&mut reply, None).await?;
        self.initialized = true;
        Ok(())
    }

    pub async fn list_tools(&mut self, cursor: Option<&str>) -> Result<ToolPage, McpError> {
        self.require_tools()?;
        if cursor.is_some_and(|cursor| cursor.len() > 8192) {
            return Err(McpError::TooLarge);
        }
        let params = cursor.map_or(json!({}), |cursor| json!({"cursor":cursor}));
        let deadline = Instant::now() + self.transport.limits.timeout;
        let result = within(deadline, self.exchange("tools/list", params, true)).await?;
        let page: ToolPage = serde_json::from_value(result).map_err(|_| McpError::Protocol)?;
        let mut names = std::collections::BTreeSet::new();
        if page.tools.len() > 1000
            || page
                .next_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.len() > 8192)
            || page.tools.iter().any(|tool| {
                tool.name.is_empty()
                    || tool.name.len() > 1024
                    || !tool.input_schema.is_object()
                    || !names.insert(&tool.name)
            })
        {
            return Err(McpError::Protocol);
        }
        Ok(page)
    }

    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: Map<String, Value>,
    ) -> Result<ToolResult, McpError> {
        self.require_tools()?;
        if name.is_empty() || name.len() > 1024 {
            return Err(McpError::Protocol);
        }
        let deadline = Instant::now() + self.transport.limits.timeout;
        let result = within(
            deadline,
            self.exchange(
                "tools/call",
                json!({"name":name,"arguments":arguments}),
                true,
            ),
        )
        .await?;
        let result: ToolResult = serde_json::from_value(result).map_err(|_| McpError::Protocol)?;
        if result
            .content
            .iter()
            .any(|item| !item.get("type").is_some_and(Value::is_string))
        {
            return Err(McpError::Protocol);
        }
        Ok(result)
    }

    fn require_tools(&self) -> Result<(), McpError> {
        if !self.initialized {
            Err(McpError::NotInitialized)
        } else if !self.tools {
            Err(McpError::ToolsUnavailable)
        } else {
            Ok(())
        }
    }

    fn check_session(&self, session: Option<&str>) -> Result<(), McpError> {
        if session.is_some() && session != self.session.as_deref() {
            Err(McpError::Protocol)
        } else {
            Ok(())
        }
    }

    async fn exchange(
        &mut self,
        method: &str,
        params: Value,
        negotiated: bool,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(McpError::Protocol)?;
        let body =
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .map_err(|_| McpError::Protocol)?;
        let mut reply = self
            .transport
            .post(body, self.session.as_deref(), negotiated, false)
            .await?;
        if negotiated {
            self.check_session(reply.session.as_deref())?;
        }
        let result = self
            .transport
            .bounded_body(&mut reply, Some(id))
            .await?
            .ok_or(McpError::Protocol)?;
        if !negotiated {
            self.session = reply.session;
        }
        Ok(result)
    }
}

macro_rules! redacted_debug {
    ($($name:ty),+) => {$(impl fmt::Debug for $name {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(concat!(stringify!($name), "([redacted])")) }
    })+};
}
redacted_debug!(McpHttpClient, ToolPage, ToolDefinition, ToolResult);
