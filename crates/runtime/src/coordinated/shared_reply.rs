use foundry_tools::host;
use std::{fmt, sync::Arc};

/// One immutable helper allocation shared with persistence; never a native seal.
#[derive(Clone)]
pub struct SharedToolReply(Arc<host::Reply>);
impl SharedToolReply {
    pub fn new(reply: host::Reply) -> Self {
        Self(Arc::new(reply))
    }
    pub fn reply(&self) -> &host::Reply {
        &self.0
    }
}
impl fmt::Debug for SharedToolReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SharedToolReply(<private>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sharing_preserves_allocation_and_debug_withholds_private_reply() {
        let value = SharedToolReply::new(host::Reply {
            version: 1,
            attempt_id: "secret-attempt".into(),
            call_id: "secret-call".into(),
            result: host::Outcome::Ok {
                output: serde_json::json!({"content":"secret-body"}),
            },
        });
        let clone = value.clone();
        assert!(std::ptr::eq(value.reply(), clone.reply()));
        assert_eq!(format!("{value:?}"), "SharedToolReply(<private>)");
        assert!(
            !format!("{:?}", crate::coordinated::ToolEvidence::SharedReply(value))
                .contains("secret")
        );
    }
}
