use serde_json::{Value, json};

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object", "properties":properties, "required":required,
           "additionalProperties":false})
}

/// The advertised schemas are also used to validate arguments before dispatch.
pub fn list_tools() -> Value {
    let text = json!({"type":"string", "minLength":1});
    let prompt = json!({"type":"string", "minLength":1, "maxLength":65536});
    let entries = [
        (
            "session_create",
            "Create a local session",
            object(
                json!({
                    "prompt":prompt,"directory":text,"idempotency_key":text
                }),
                &["prompt", "directory"],
            ),
        ),
        (
            "session_list",
            "List local sessions",
            object(json!({}), &[]),
        ),
        (
            "session_get",
            "Read a session",
            object(json!({"session_id":text}), &["session_id"]),
        ),
        (
            "session_send",
            "Send a message to a session",
            object(
                json!({
                    "session_id":text,"message":prompt,"message_id":text
                }),
                &["session_id", "message", "message_id"],
            ),
        ),
        (
            "session_cancel",
            "Cancel a session",
            object(
                json!({
                    "session_id":text,"mode":{"type":"string","enum":["graceful","hard"]}
                }),
                &["session_id", "mode"],
            ),
        ),
    ];
    Value::Array(entries.into_iter().map(|(name, description, schema)| {
        json!({"name":name,"description":description,"inputSchema":schema})
    }).collect())
}

pub(crate) fn validate_arguments(name: &str, args: &Value) -> Result<(), &'static str> {
    let tools = list_tools();
    let tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .ok_or("Unknown tool")?;
    let schema = &tool["inputSchema"];
    let args = args.as_object().ok_or("Arguments must be an object")?;
    let properties = schema["properties"].as_object().unwrap();
    if schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|key| !args.contains_key(key.as_str().unwrap()))
    {
        return Err("Missing required argument");
    }
    for (key, value) in args {
        let rule = properties.get(key).ok_or("Unknown argument")?;
        let text = value.as_str().ok_or("Argument must be a string")?;
        let length = text.chars().count() as u64;
        if length < rule["minLength"].as_u64().unwrap_or(0)
            || length > rule["maxLength"].as_u64().unwrap_or(u64::MAX)
        {
            return Err("Argument length outside schema bounds");
        }
        if rule["enum"]
            .as_array()
            .is_some_and(|choices| !choices.contains(value))
        {
            return Err("Unknown argument value");
        }
    }
    Ok(())
}
