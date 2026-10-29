use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContextId(String);
impl ContextId {
    pub fn new(value: impl Into<String>) -> Result<Self, ContextError> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(ContextError::Identifier);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ContextId {
    type Error = ContextError;
    fn try_from(v: String) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}
impl From<ContextId> for String {
    fn from(v: ContextId) -> Self {
        v.0
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContextReceipt(String);
impl ContextReceipt {
    pub fn new(value: impl Into<String>) -> Result<Self, ContextError> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.len() != 36
            || bytes[14] != b'4'
            || !b"89ab".contains(&bytes[19])
            || !bytes.iter().enumerate().all(|(i, b)| {
                if [8, 13, 18, 23].contains(&i) {
                    *b == b'-'
                } else {
                    b.is_ascii_digit() || (b'a'..=b'f').contains(b)
                }
            })
        {
            return Err(ContextError::Identifier);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ContextReceipt {
    type Error = ContextError;
    fn try_from(v: String) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}
impl From<ContextReceipt> for String {
    fn from(v: ContextReceipt) -> Self {
        v.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContextDigest([u8; 32]);
impl ContextDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
    pub fn parse(value: &str) -> Result<Self, ContextError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ContextError::Digest);
        }
        let mut digest = [0; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let digit = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b - b'a' + 10
                }
            };
            digest[index] = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Ok(Self(digest))
    }
    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}
impl TryFrom<String> for ContextDigest {
    type Error = ContextError;
    fn try_from(v: String) -> Result<Self, Self::Error> {
        Self::parse(&v)
    }
}
impl From<ContextDigest> for String {
    fn from(v: ContextDigest) -> Self {
        v.to_hex()
    }
}

pub struct ContextPayload(Vec<u8>);
impl ContextPayload {
    pub fn new(bytes: Vec<u8>) -> Result<Self, ContextError> {
        if bytes.is_empty() || bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(ContextError::Limit);
        }
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
    pub fn digest(&self) -> ContextDigest {
        ContextDigest::of(&self.0)
    }
    pub(crate) fn encoded(&self) -> String {
        STANDARD.encode(&self.0)
    }
    pub(crate) fn decode(encoded: &str) -> Result<Self, ContextError> {
        if encoded.is_empty() || encoded.len() > MAX_BASE64_BYTES {
            return Err(ContextError::Limit);
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| ContextError::Encoding)?;
        if STANDARD.encode(&bytes) != encoded {
            return Err(ContextError::Encoding);
        }
        Self::new(bytes)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextFormat {
    pub profile: ContextId,
    pub format_version: ContextId,
    pub producer_build: ContextId,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputCoverage {
    pub input_id: ContextId,
    pub input_digest: ContextDigest,
    pub native_ids: Vec<ContextId>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultCoverage {
    pub attempt_id: ContextId,
    pub call_id: ContextId,
    pub result_digest: ContextDigest,
    pub native_ids: Vec<ContextId>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCoverage {
    pub completed_turn_id: ContextId,
    pub inputs: Vec<InputCoverage>,
    pub results: Vec<ResultCoverage>,
}

pub(super) fn metadata(
    format: &ContextFormat,
    coverage: Option<&ContextCoverage>,
) -> Result<(), ContextError> {
    bounded_json(format, MAX_FORMAT_BYTES)?;
    if let Some(coverage) = coverage {
        if coverage.inputs.len() > 1024
            || coverage.results.len() > 1024
            || coverage
                .inputs
                .iter()
                .any(|v| v.native_ids.is_empty() || v.native_ids.len() > 1024)
            || coverage
                .results
                .iter()
                .any(|v| v.native_ids.is_empty() || v.native_ids.len() > 1024)
        {
            return Err(ContextError::Limit);
        }
        bounded_json(coverage, MAX_COVERAGE_BYTES)?;
    }
    Ok(())
}

pub(super) fn bounded_json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, ContextError> {
    struct Output {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Output {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Output {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value).map_err(|_| ContextError::Limit)?;
    Ok(output.bytes)
}
