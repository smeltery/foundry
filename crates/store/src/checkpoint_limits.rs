use crate::{Result, conflict};
use rusqlite::Connection;
const SESSION_BYTES: i64 = 256 * 1024 * 1024;
const DATABASE_BYTES: i64 = 1024 * 1024 * 1024;
// Payload plus logical metadata, identities, digests and integer columns; no page/WAL estimate.
const CHARGE_SQL: &str = "length(payload)+length(CAST(metadata AS BLOB))
    +length(CAST(coverage AS BLOB))+length(CAST(id AS BLOB))
    +length(CAST(session_id AS BLOB))+length(CAST(attempt_id AS BLOB))
    +length(CAST(request_id AS BLOB))+COALESCE(length(CAST(parent_id AS BLOB)),0)+152
    +COALESCE((SELECT 76 FROM context_finalizations WHERE checkpoint_id=context_checkpoints.id),0)
    +COALESCE((SELECT length(body)+184 FROM context_witnesses WHERE checkpoint_id=context_checkpoints.id),0)";

/// Shared by every checkpoint publication and Continue acceptance transaction.
pub(crate) fn check(db: &Connection, session: &str, addition: i64) -> Result<()> {
    crate::witness_storage::check_quota_associations(db)?;
    let query = format!(
        "SELECT COALESCE(SUM(CASE WHEN session_id=?1 THEN {CHARGE_SQL} ELSE 0 END),0), COALESCE(SUM({CHARGE_SQL}),0) FROM context_checkpoints"
    );
    let (mut session_bytes, mut total_bytes): (i64, i64) =
        db.query_row(&query, [session], |r| Ok((r.get(0)?, r.get(1)?)))?;
    // Do not inner-join away missing parents/events or charge only the current session.
    let mut statement = db.prepare("SELECT a.restore_id,r.session_id FROM context_continue_acceptances a LEFT JOIN context_restores r ON r.id=a.restore_id")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let restore = row
            .get_ref(0)?
            .as_str()
            .map_err(|_| conflict("invalid acceptance quota identity"))?;
        let owner = row
            .get_ref(1)?
            .as_str()
            .map_err(|_| conflict("missing acceptance quota owner"))?;
        let verified = crate::continue_read::verified(db, owner, restore)?
            .ok_or_else(|| conflict("missing acceptance quota association"))?;
        let body = crate::continue_codec::encode(&verified.input)?;
        let event =
            crate::continue_codec::event(&verified.input, verified.reference.request_digest)?;
        let charge = acceptance_charge(&verified.input, body.len(), event.len())?;
        total_bytes = total_bytes
            .checked_add(charge)
            .ok_or_else(|| conflict("checkpoint quota overflow"))?;
        if owner == session {
            session_bytes = session_bytes
                .checked_add(charge)
                .ok_or_else(|| conflict("checkpoint quota overflow"))?;
        }
    }
    quota(session_bytes, total_bytes, addition)
}
pub(crate) fn acceptance_charge(
    input: &crate::ContinueAcceptanceInput,
    body: usize,
    event: usize,
) -> Result<i64> {
    [
        body,
        116,
        input.intent.session_id.len(),
        input.authorization.attempt_id.len(),
        "continue-accepted:".len() + input.intent.restore_id.len(),
        "continue_accepted".len(),
        event,
        16,
    ]
    .into_iter()
    .try_fold(0_i64, |sum, n| {
        sum.checked_add(i64::try_from(n).ok()?).filter(|v| *v >= 0)
    })
    .ok_or_else(|| conflict("acceptance quota overflow"))
}

fn quota(session: i64, total: i64, addition: i64) -> Result<()> {
    if session < 0
        || total < 0
        || addition < 0
        || session
            .checked_add(addition)
            .is_none_or(|n| n > SESSION_BYTES)
        || total
            .checked_add(addition)
            .is_none_or(|n| n > DATABASE_BYTES)
    {
        return Err(conflict("checkpoint quota exceeded"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_limits_are_inclusive_and_overflow_is_rejected() {
        assert!(quota(SESSION_BYTES - 1, DATABASE_BYTES - 1, 1).is_ok());
        assert!(quota(SESSION_BYTES, DATABASE_BYTES - 1, 1).is_err());
        assert!(quota(0, DATABASE_BYTES, 1).is_err());
        assert!(quota(i64::MAX, 0, 1).is_err());
        assert!(quota(0, i64::MAX, 1).is_err());
        assert!(quota(-1, 0, 1).is_err());
    }
}
