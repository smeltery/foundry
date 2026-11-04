//! Long-poll event delivery. SQLite has no cross-process notification, so a
//! waiting reader checks the connection's commit marker, which changes when any
//! other connection or process commits, and reads again only after it moves.
use crate::{Result, Service};
use foundry_store::{EventPage, EventRead};
use std::time::{Duration, Instant};

const COMMIT_CHECK_INTERVAL: Duration = Duration::from_millis(20);

impl Service {
    /// Returns as soon as events exist, the session cannot grow, or the wait budget ends.
    /// An empty page is not terminal; readers continue until the lifecycle settles.
    pub fn read_events(&mut self, session_id: &str, read: &EventRead) -> Result<EventPage> {
        let deadline = Instant::now() + read.wait()?;
        loop {
            let marker = self.store.commit_marker()?;
            let page = self.store.deliver_events(session_id, read)?;
            if !page.events.is_empty() || !page.may_grow() {
                return Ok(page);
            }
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(page);
                }
                std::thread::sleep(remaining.min(COMMIT_CHECK_INTERVAL));
                if self.store.commit_marker()? != marker {
                    break;
                }
            }
        }
    }
}
