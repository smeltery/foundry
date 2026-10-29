//! Explicit, lossless v1 migration; normal open never invokes this path.
use super::*;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Legacy {
    version: u32,
    incarnation: [u8; 32],
    identity: EndpointId,
    policy: [u8; 32],
    revision: u64,
    parent: [u8; 32],
    accepted_ms: i64,
    document: Option<TeamDocument>,
    intents: Vec<()>,
}
pub(super) fn migrate(
    connection: &mut Connection,
    identity: &LocalIdentity,
    policy: &StorePolicy,
    now: i64,
    protection: &Protection,
) -> Result<()> {
    if policy.codes.is_some() {
        return Err(StoreError::Unsupported);
    }
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let bytes = checkpoint::read_bytes(&tx, identity, 1)?;
    let old: Legacy = serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt)?;
    if old.version != 1 || old.revision > i64::MAX as u64 || checkpoint::encode(&old)? != bytes {
        return Err(StoreError::Corrupt);
    }
    if old.identity != identity.endpoint_id() || old.policy != policy.digest {
        return Err(StoreError::Binding);
    }
    if !old.intents.is_empty() {
        return Err(StoreError::Unsupported);
    }
    let mut next = Checkpoint::empty(old.incarnation, old.identity, old.policy);
    next.revision = old.revision;
    next.parent = old.parent;
    next.accepted_ms = old.accepted_ms;
    next.operation_ms = old.accepted_ms;
    next.state = old
        .document
        .map_or(StoredState::Empty, |document| StoredState::Active {
            document: Box::new(document),
        });
    validation::validate(&next, policy, now)?;
    checkpoint::write(&tx, &next, identity, false)?;
    tx.pragma_update(None, "user_version", 2)?;
    #[cfg(test)]
    if FAIL_AFTER_WRITE.with(|flag| flag.replace(false)) {
        return Err(StoreError::Storage);
    }
    protection.check()?;
    tx.commit().map_err(|_| StoreError::Indeterminate)?;
    Ok(())
}

#[cfg(test)]
thread_local! { pub(super) static FAIL_AFTER_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; pub(super) static FAIL_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(super) fn v1_fixture(store: &FullTeamStore) -> [u8; 32] {
    let (current, _) = checkpoint::read(&store.connection, &store.identity, &store.policy).unwrap();
    let old = Legacy {
        version: 1,
        incarnation: current.incarnation,
        identity: store.identity.endpoint_id(),
        policy: store.policy.digest,
        revision: current.revision,
        parent: current.parent,
        accepted_ms: current.accepted_ms,
        document: match current.state {
            StoredState::Active { document } => Some(*document),
            StoredState::Empty => None,
            _ => panic!("unsupported fixture"),
        },
        intents: Vec::new(),
    };
    let bytes = checkpoint::encode(&old).unwrap();
    let signature = store
        .identity
        .secret_key()
        .sign(&checkpoint::preimage(&bytes, 1));
    store
        .connection
        .execute(
            "UPDATE checkpoint SET payload=?1,signature=?2",
            rusqlite::params![bytes, signature.to_bytes().as_slice()],
        )
        .unwrap();
    store
        .connection
        .pragma_update(None, "user_version", 1)
        .unwrap();
    checkpoint::digest(&bytes)
}
