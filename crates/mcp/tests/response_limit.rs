use foundry_mcp::{Backend, MAX_FRAME_BYTES, McpError, serve};
use serde_json::{Value, json};
use std::io::{Cursor, ErrorKind};

struct Unused;
impl Backend for Unused {
    fn call(&mut self, _: &str, _: Value) -> Result<Value, McpError> {
        panic!("transport error must not dispatch tools")
    }
}

#[test]
fn oversized_error_id_terminates_with_a_bounded_uncorrelated_frame() {
    let mut request = json!({"jsonrpc":"2.0","id":"","method":"unknown"});
    let overhead = serde_json::to_vec(&request).unwrap().len();
    request["id"] = Value::String("x".repeat(MAX_FRAME_BYTES - overhead));
    let mut input = serde_json::to_vec(&request).unwrap();
    assert_eq!(input.len(), MAX_FRAME_BYTES);
    input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n");
    let mut output = Vec::new();
    let error = serve(&mut Cursor::new(input), &mut output, &mut Unused).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(output.len() <= MAX_FRAME_BYTES + 1);
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
    let response: Value = serde_json::from_slice(&output).unwrap();
    assert!(response["id"].is_null());
    assert_eq!(response["error"]["code"], -32603);
}
