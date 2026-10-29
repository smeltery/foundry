use super::*;
use serde::Serialize;

pub enum AdapterCredential {
    None,
    ApiKey { provider: String, value: String },
}

/// Explicit private inputs; no environment or account discovery.
pub struct AdapterPrivateInputs {
    pub credential: AdapterCredential,
    pub system_prompt: String,
    pub agent_directory: PathBuf,
}

impl AdapterPrivateInputs {
    pub(super) fn encode(self, selection: &AdapterSelection) -> Result<Vec<u8>> {
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Credential<'a> {
            None,
            ApiKey { provider: &'a str, value: &'a str },
        }
        let credential = match &self.credential {
            AdapterCredential::None => Credential::None,
            AdapterCredential::ApiKey { provider, value } => {
                if provider != &selection.provider || value.is_empty() || value.len() > 16_384 {
                    return Err(invalid());
                }
                Credential::ApiKey { provider, value }
            }
        };
        let directory = self.agent_directory.to_str().ok_or_else(invalid)?;
        if !self.agent_directory.is_absolute()
            || directory.len() > 4096
            || directory.contains('\0')
            || self.system_prompt.len() > 16_384
        {
            return Err(invalid());
        }
        #[derive(Serialize)]
        struct Private<'a> {
            credential: Credential<'a>,
            system_prompt: &'a str,
            agent_directory: &'a str,
        }
        #[derive(Serialize)]
        struct Body<'a> {
            version: u32,
            selection: &'a AdapterSelection,
            private: Private<'a>,
        }
        serde_json::to_vec(&Body {
            version: 1,
            selection,
            private: Private {
                credential,
                system_prompt: &self.system_prompt,
                agent_directory: directory,
            },
        })
        .map_err(|_| invalid())
    }
}
