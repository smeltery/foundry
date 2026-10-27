#![allow(dead_code)]
#[path = "../persistence_support/mod.rs"]
mod base;
pub use base::{NOW, WAIT};
use foundry_network::{
    LocalIdentity,
    full_team_store::{FullTeamStore, StorePolicy},
    team_state::Formation,
};
use std::sync::Arc;
pub struct Fixture {
    pub base: base::Fixture,
    pub identity: Arc<LocalIdentity>,
}
impl Fixture {
    pub fn new() -> Self {
        let base = base::Fixture::new();
        let dir =
            cap_std::fs::Dir::open_ambient_dir(&base.root, cap_std::ambient_authority()).unwrap();
        let identity = Arc::new(foundry_network::load_or_create_identity(&dir, WAIT).unwrap());
        Self { base, identity }
    }
    pub fn policy(&self) -> StorePolicy {
        StorePolicy::new(self.base.profile.clone(), None).unwrap()
    }
    pub fn create(&self) -> FullTeamStore {
        FullTeamStore::create_new(&self.base.root, self.identity.clone(), self.policy(), WAIT)
            .unwrap()
    }
    pub fn open(&self) -> FullTeamStore {
        FullTeamStore::open_existing(&self.base.root, self.identity.clone(), self.policy(), WAIT)
            .unwrap()
    }
}
pub fn formation() -> Formation<'static> {
    Formation {
        name: "group",
        person: "alice",
        machine: "desk",
    }
}
