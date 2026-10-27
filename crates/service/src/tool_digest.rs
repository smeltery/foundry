use crate::Result;
use foundry_store::StoreError;
use foundry_tools::host::{Input, Reply};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Compact serialization of decoded input only, not the typed ledger invocation.
/// Stream bytes into the hash so no second raw-content buffer is retained.
pub(crate) fn event_input(input: &Value) -> Result<EventInputDigest> {
    struct Sink {
        hash: Sha256,
        bytes: usize,
    }
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("input byte count overflow"))?;
            self.hash.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        hash: Sha256::new(),
        bytes: 0,
    };
    serde_json::to_writer(&mut sink, input).map_err(StoreError::Json)?;
    let sha256 = sink
        .hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(EventInputDigest {
        bytes: sink.bytes,
        sha256,
    })
}

pub(crate) struct EventInputDigest {
    pub bytes: usize,
    pub sha256: String,
}

/// Versioned, fixed-field encoding of the validated dispatch input, not raw JSON.
pub(crate) fn input(tool: &str, input: &Input) -> Result<[u8; 32]> {
    #[derive(Serialize)]
    struct InputDigest<'a> {
        tool: &'a str,
        input: &'a Input,
    }
    hash(b"foundry.tool.input.v1\0", &InputDigest { tool, input })
}

pub(crate) fn output(reply: &Reply) -> Result<[u8; 32]> {
    let value = serde_json::to_value(reply).map_err(StoreError::Json)?;
    hash(b"foundry.tool.output.v1\0", &lexical(value))
}

fn hash(domain: &[u8], value: &impl Serialize) -> Result<[u8; 32]> {
    let bytes = serde_json::to_vec(value).map_err(StoreError::Json)?;
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(bytes);
    Ok(digest.finalize().into())
}

fn lexical(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut fields: Vec<_> = object.into_iter().collect();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key, lexical(value)))
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.into_iter().map(lexical).collect()),
        scalar => scalar,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_tools::host::{Outcome, PathArgs, VERSION};

    fn hex(digest: [u8; 32]) -> String {
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn input_has_a_stable_versioned_encoding() {
        let args = Input::Path(PathArgs {
            path: "note.txt".into(),
        });
        assert_eq!(
            hex(input("read", &args).unwrap()),
            "52ef150c8a96151377bde3538f4c49fc5df8b4866eb0e5af793bcf1c16994d38"
        );
        assert_ne!(input("read", &args).unwrap(), input("list", &args).unwrap());
    }

    #[test]
    fn output_sorts_nested_objects_without_reordering_arrays() {
        let make = |text| Reply {
            version: VERSION,
            attempt_id: "attempt".into(),
            call_id: "call".into(),
            result: Outcome::Ok {
                output: serde_json::from_str(text).unwrap(),
            },
        };
        let first = make(r#"{"z":[{"b":2,"a":1},3],"a":true}"#);
        let second = make(r#"{"a":true,"z":[{"a":1,"b":2},3]}"#);
        assert_eq!(output(&first).unwrap(), output(&second).unwrap());
        assert_eq!(
            hex(output(&first).unwrap()),
            "1a3a0c9eb99a652eebe3543420f5cef113e1206de54d48658bbfb4f9a25370ba"
        );
        assert_ne!(
            output(&first).unwrap(),
            output(&make(r#"{"a":true,"z":[3,{"a":1,"b":2}]}"#)).unwrap()
        );
    }
}
