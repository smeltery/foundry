#![allow(dead_code)]
use foundry_network::{
    WireProfile,
    membership::{MembershipSnapshot, MembershipState},
    persistence::{MEMBERSHIP_DATABASE, MembershipStore},
    signing::{MemberRecord, MetadataRecord, SignedRecord},
};
use iroh::SecretKey;
use std::{collections::BTreeMap, fs, os::unix::fs::DirBuilderExt, path::PathBuf, time::Duration};
pub const NOW: i64 = 100;
pub const WAIT: Duration = Duration::from_millis(100);
pub struct Fixture {
    pub root: PathBuf,
    pub founder: SecretKey,
    pub profile: WireProfile,
}
impl Fixture {
    pub fn new() -> Self {
        let founder = SecretKey::generate();
        let root = std::env::temp_dir().join(format!("projection-{}", founder.public()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let profile = WireProfile {
            team_alpn: b"fixture/team".to_vec(),
            join_alpn: b"fixture/join".to_vec(),
            protocol: 1,
            signing_prefix: "fixture/sign/".into(),
            invitation_endpoint_context: "fixture endpoint".into(),
            invitation_secret_context: "fixture secret".into(),
            ticket_prefixes: vec!["fixture-".into()],
        };
        Self {
            root,
            founder,
            profile,
        }
    }
    pub fn member(
        &self,
        endpoint: &SecretKey,
        active: bool,
        ts: i64,
    ) -> SignedRecord<MemberRecord> {
        SignedRecord::sign(
            MemberRecord {
                endpoint: endpoint.public(),
                person: endpoint.public().to_string()[..12].into(),
                machine: "desk".into(),
                active,
                ts,
            },
            &self.founder,
            "team",
            &self.profile,
        )
        .unwrap()
    }
    pub fn initial(&self) -> MembershipSnapshot {
        let record = self.member(&self.founder, true, 1);
        MembershipSnapshot {
            team: "team".into(),
            founder: self.founder.public(),
            metadata: SignedRecord::sign(
                MetadataRecord {
                    name: "group".into(),
                    relay: None,
                    ts: 1,
                },
                &self.founder,
                "team",
                &self.profile,
            )
            .unwrap(),
            admissions: BTreeMap::from([(self.founder.public().to_string(), record.clone())]),
            members: BTreeMap::from([(self.founder.public().to_string(), record)]),
        }
    }
    pub fn state(&self, snapshot: &MembershipSnapshot) -> MembershipState {
        MembershipState::bootstrap("team", self.founder.public(), snapshot, &self.profile, NOW)
            .unwrap()
            .0
    }
    pub fn open(&self) -> MembershipStore {
        MembershipStore::open(&self.root, WAIT).unwrap()
    }
    pub fn initialized(&self) -> MembershipStore {
        let mut store = self.open();
        store
            .initialize(&self.state(&self.initial()), &self.profile, NOW)
            .unwrap();
        store
    }
    pub fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.root.join(MEMBERSHIP_DATABASE)).unwrap()
    }
    pub fn update(&self, ts: i64) -> MembershipSnapshot {
        let mut value = self.initial();
        value.metadata = SignedRecord::sign(
            MetadataRecord {
                name: format!("update-{ts}"),
                relay: None,
                ts,
            },
            &self.founder,
            "team",
            &self.profile,
        )
        .unwrap();
        value
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
