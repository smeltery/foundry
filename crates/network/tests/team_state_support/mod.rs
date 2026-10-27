#![allow(dead_code)]
use foundry_network::{
    WireProfile,
    team_state::{Admission, Entropy, Formation, Result, TeamError, TeamState},
};
use iroh::SecretKey;
pub const NOW: i64 = 100;
pub struct FixedEntropy(pub u8);
impl Entropy for FixedEntropy {
    fn fill(&mut self, bytes: &mut [u8]) -> Result<()> {
        bytes.fill(self.0);
        self.0 = self.0.wrapping_add(1);
        Ok(())
    }
}
pub struct FailedEntropy;
impl Entropy for FailedEntropy {
    fn fill(&mut self, _: &mut [u8]) -> Result<()> {
        Err(TeamError::Entropy)
    }
}
pub struct Fixture {
    pub owner: SecretKey,
    pub profile: WireProfile,
    pub state: TeamState,
}
impl Fixture {
    pub fn new() -> Self {
        let owner = SecretKey::from_bytes(&[1; 32]);
        let profile = WireProfile {
            team_alpn: b"fixture/team".to_vec(),
            join_alpn: b"fixture/join".to_vec(),
            protocol: 1,
            signing_prefix: "fixture/sign/".into(),
            invitation_endpoint_context: "fixture endpoint".into(),
            invitation_secret_context: "fixture secret".into(),
            ticket_prefixes: vec!["fixture-".into()],
        };
        let state = TeamState::found(
            Formation {
                name: "group",
                person: "alice",
                machine: "desk",
            },
            &owner,
            &profile,
            NOW,
            &mut FixedEntropy(1),
        )
        .unwrap();
        Self {
            owner,
            profile,
            state,
        }
    }
    pub fn secret<'a>(&self, state: &'a TeamState) -> &'a str {
        &state.document().link.as_ref().unwrap().body.secret
    }
    pub fn admit(
        &self,
        state: &TeamState,
        actor: &SecretKey,
        key: &SecretKey,
        person: &str,
        secret: &str,
        now: i64,
    ) -> Result<TeamState> {
        state.admit(
            actor,
            Admission {
                endpoint: key.public(),
                person,
                machine: "desk",
                secret,
                proof: None,
                owner_only: false,
            },
            now,
        )
    }
    pub fn boot(&self, document: &foundry_network::team::TeamDocument) -> TeamState {
        TeamState::bootstrap(
            &document.id,
            self.owner.public(),
            document,
            &self.profile,
            NOW,
        )
        .unwrap()
        .0
    }
}
pub fn key(n: u8) -> SecretKey {
    SecretKey::from_bytes(&[n; 32])
}
pub fn hash(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}
