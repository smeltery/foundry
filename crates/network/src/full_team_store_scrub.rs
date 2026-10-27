use super::*;
impl FullTeamStore {
    pub fn retry_scrub(&mut self, receipt: &RemovalReceipt) -> Result<ScrubStatus> {
        if !Arc::ptr_eq(&self.instance, &receipt.instance) {
            return Err(StoreError::Binding);
        }
        let _guard = self.lock.lock(self.timeout)?;
        self.protection.check()?;
        let (current, digest) = checkpoint::read(&self.connection, &self.identity, &self.policy)?;
        if current.incarnation != self.instance.incarnation
            || digest != receipt.digest
            || current.revision != receipt.revision
            || !matches!(current.state, StoredState::Removed { .. })
        {
            return Err(StoreError::Conflict);
        }
        let result = self
            .connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            });
        let columns = match result {
            Ok(columns) => columns,
            Err(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                return Ok(ScrubStatus::PendingBusy);
            }
            Err(_) => return Ok(ScrubStatus::Failed(ScrubFailure::Storage)),
        };
        if columns.0 != 0 {
            return Ok(ScrubStatus::PendingBusy);
        }
        if columns != (0, 0, 0) {
            return Ok(ScrubStatus::Failed(ScrubFailure::UnexpectedResult));
        }
        if self.protection.check().is_err() {
            return Ok(ScrubStatus::Failed(ScrubFailure::Protection));
        }
        let wal = self
            .protection
            .path()
            .with_file_name(format!("{FULL_TEAM_DATABASE}-wal"));
        match std::fs::symlink_metadata(wal) {
            Ok(metadata) if metadata.len() == 0 => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Ok(ScrubStatus::Failed(ScrubFailure::UnexpectedResult)),
            Err(_) => return Ok(ScrubStatus::Failed(ScrubFailure::Storage)),
        }
        #[cfg(test)]
        if self.fail_scrub_sync {
            return Ok(ScrubStatus::Failed(ScrubFailure::Sync));
        }
        if self.protection.sync_database().is_err() {
            return Ok(ScrubStatus::Failed(ScrubFailure::Sync));
        }
        Ok(ScrubStatus::Complete)
    }
}
