use super::{Error, Registration, Result, json};
use foundry_contracts::context::ContextDigest;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{process::Command, time::Instant};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Configure,
    Prepare,
    Observe,
    Publish,
}
#[derive(Serialize)]
struct Request<'a> {
    version: u8,
    request_id: &'a str,
    operation: Operation,
    registration_digest: ContextDigest,
    input: &'a Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    version: u8,
    request_id: String,
    operation: Operation,
    registration_digest: ContextDigest,
    request_digest: ContextDigest,
    result: Verdict,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Verdict {
    Ok { value: Value },
    Error { code: Code },
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Code {
    Unsupported,
    Invalid,
    Policy,
    Mapping,
    Cancelled,
    Deadline,
    Internal,
}
impl Registration {
    pub(super) fn call<T: DeserializeOwned>(
        &self,
        operation: Operation,
        input: &Value,
        deadline: Instant,
        cancelled: impl Fn() -> bool,
    ) -> Result<T> {
        let id = uuid::Uuid::new_v4().to_string();
        let bytes = json::encode(
            &Request {
                version: 1,
                request_id: &id,
                operation,
                registration_digest: self.options.registration_digest,
                input,
            },
            8 * 1048576,
        )?;
        json::decode(&bytes, 8 * 1048576)?;
        #[cfg(unix)]
        let output = {
            let mut command = Command::new(&self.options.executable);
            command.arg("--foundry-profile-host-v1").env_clear();
            super::process::run_with_output_limit(
                &mut command,
                &bytes,
                deadline,
                &cancelled,
                2 * 1048576,
            )?
        };
        #[cfg(not(unix))]
        let output: Vec<u8> = return Err(Error::Unsupported);
        let value = json::decode(&output, 2 * 1048576)?;
        let reply: Reply = serde_json::from_value(value).map_err(|_| Error::Invalid)?;
        if reply.version != 1
            || reply.request_id != id
            || reply.operation != operation
            || reply.registration_digest != self.options.registration_digest
            || reply.request_digest != ContextDigest::of(&bytes)
            || cancelled()
            || Instant::now() >= deadline
        {
            return Err(Error::Invalid);
        }
        match reply.result {
            Verdict::Ok { value } => serde_json::from_value(value).map_err(|_| Error::Invalid),
            Verdict::Error { code } => Err(match code {
                Code::Unsupported => Error::Unsupported,
                Code::Invalid => Error::Invalid,
                Code::Policy => Error::Policy,
                Code::Mapping => Error::Mapping,
                Code::Cancelled => Error::Cancelled,
                Code::Deadline => Error::Deadline,
                Code::Internal => Error::Internal,
            }),
        }
    }
}
