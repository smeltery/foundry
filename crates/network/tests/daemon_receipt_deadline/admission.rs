use super::{DEADLINE, client::Client, config::Config};
use cap_std::{ambient_authority, fs::Dir};
use foundry_network::{
    EndpointConfig, NetworkEndpoint, RelaySelection, WireProfile,
    daemon_lease::DaemonLease,
    full_team_store::{CommitOutcome, FullTeamStore, JoinTrust, StorePolicy},
    join_protocol::{JoinResult, join_peer},
    mail_store::{DeliveryIntent, Insert, MailStore, Message, PayloadEncoding, State},
    team::{JoinRequest, TeamDocument},
    wire,
};
use iroh::EndpointId;
use serde_json::json;
use std::sync::Arc;

pub struct Admitted {
    pub endpoint: NetworkEndpoint,
    pub profile: WireProfile,
    pub remote: EndpointId,
    pub team: String,
    pub presence: wire::Presence,
    pub store: FullTeamStore,
    pub mail: MailStore,
    pub ticket: String,
    pub bearer: String,
    _lease: DaemonLease,
}
pub fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}
impl Admitted {
    pub async fn join(client: &mut Client, auth: &serde_json::Value, config: &Config) -> Self {
        let value = client
            .control(
                auth,
                json!({"kind":"invite","short":false,"hours":1,"own":false}),
            )
            .await;
        let ticket = value["ticket"].as_str().unwrap().to_owned();
        let address: iroh::EndpointAddr =
            serde_json::from_value(value["endpoint"].clone()).unwrap();
        let profile: WireProfile = serde_json::from_value(super::config::profile()).unwrap();
        let decoded = foundry_network::ticket::TicketCodec::new(&profile, None)
            .unwrap()
            .decode(&ticket)
            .unwrap();
        assert!(decoded.peers.contains(&address.id));
        let directory = Dir::open_ambient_dir(&config.state, ambient_authority()).unwrap();
        let lease = DaemonLease::acquire(&directory).unwrap();
        let identity =
            Arc::new(foundry_network::load_or_create_identity(&directory, DEADLINE).unwrap());
        let endpoint = NetworkEndpoint::bind(EndpointConfig {
            profile: profile.clone(),
            secret_key: identity.secret_key().clone(),
            relay: RelaySelection::Disabled,
            bind_address: "127.0.0.1:0".parse().unwrap(),
            direct_paths: true,
            relay_root_certificates: Vec::new(),
        })
        .await
        .unwrap();
        let machine = config.value["machine"].as_str().unwrap().to_owned();
        let joined = match join_peer(
            &endpoint,
            address.clone(),
            &JoinRequest {
                team: decoded.team.clone(),
                secret: decoded.secret.clone(),
                person: config.person.clone(),
                machine: machine.clone(),
                proof: None,
                owner_only: false,
            },
            DEADLINE,
        )
        .await
        .unwrap()
        {
            JoinResult::Joined(joined) => joined,
            JoinResult::Refused(_) => panic!("actual invitation refused"),
        };
        assert_eq!(joined.peer, address.id);
        let mut store = FullTeamStore::create_new(
            &config.state,
            identity.clone(),
            StorePolicy::new(profile.clone(), None).unwrap(),
            DEADLINE,
        )
        .unwrap();
        let base = store.load(now()).unwrap();
        let pending = store
            .prepare_join(
                &base,
                &joined.state,
                JoinTrust {
                    team: &decoded.team,
                    founder: Some(address.id),
                    inviter: joined.peer,
                },
                now(),
            )
            .unwrap();
        store.commit_join(pending).unwrap();
        let local = identity.endpoint_id();
        let document = store.load(now()).unwrap();
        let local_member = &document.document().unwrap().members[&local.to_string()].body;
        assert!(
            local_member.active
                && local_member.person == config.person
                && local_member.machine == machine
        );
        let destination = format!("{}/{}/{}", config.person, machine, config.name);
        let presence = wire::Presence {
            sessions: vec![wire::SessionPresence {
                address: destination.clone(),
                name: config.name.clone(),
                person: config.person.clone(),
                machine,
                agent: "fixture".into(),
                cwd: String::new(),
                repo: None,
                branch: None,
                status: "waiting".into(),
                since: None,
                doing: None,
                tier: wire::DeliveryTier::Pull,
                unread: 0,
                is_me: false,
            }],
            known: vec![destination],
            kept: Vec::new(),
            features: Vec::new(),
        };
        Self {
            endpoint,
            profile,
            remote: joined.peer,
            team: decoded.team,
            presence,
            store,
            mail: MailStore::open(&config.state).unwrap(),
            ticket,
            bearer: decoded.secret,
            _lease: lease,
        }
    }
    pub fn state_frame(&mut self) -> foundry_network::PeerFrame {
        let loaded = self.store.load(now()).unwrap();
        let state =
            serde_json::from_value(serde_json::to_value(loaded.document().unwrap()).unwrap())
                .unwrap();
        foundry_network::PeerFrame::State {
            state: Box::new(state),
        }
    }
    pub fn merge(&mut self, state: wire::StateDocument) {
        let document: TeamDocument =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        let base = self.store.load(now()).unwrap();
        let (pending, report) = self
            .store
            .prepare_merge(&base, self.remote, &document, now())
            .unwrap();
        assert_eq!(report.rejected, 0);
        assert_eq!(report.proof_required, 0);
        assert!(matches!(
            self.store.commit_transition(pending, now()).unwrap(),
            CommitOutcome::Active(_) | CommitOutcome::Unchanged(_)
        ));
    }
    pub fn persist(&mut self, message: &wire::Message) -> (Insert, Vec<u8>) {
        let payload = serde_json::to_vec(message).unwrap();
        let record = Message {
            id: message.id.clone(),
            sender: message.from.clone(),
            recipient: message.to.clone(),
            body: payload.clone(),
            encoding: PayloadEncoding::WireJson,
            reply_to: message.reply_to.clone(),
            intent: DeliveryIntent {
                wake: message.wake,
                notify: message.notify,
            },
        };
        let inserted = self.mail.insert(&record, State::Pending).unwrap();
        (inserted, payload)
    }
}
