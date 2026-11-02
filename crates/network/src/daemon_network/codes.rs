use super::*;
use crate::{
    code_listener::{CodeListener, CodeTransport},
    full_team_store::{CodeIntentId, CodeLease, ControlEffect},
    peer_session::PeerError,
};
use tokio::{sync::Mutex, task::JoinHandle};
struct Entry {
    lease: Arc<CodeLease>,
    address: EndpointAddr,
    stop: watch::Sender<bool>,
    task: JoinHandle<crate::peer_session::Result<crate::code_listener::CodeExit>>,
}
#[derive(Default)]
pub(super) struct Codes {
    entries: Mutex<BTreeMap<CodeIntentId, Entry>>,
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Reconciled {
    Current,
    Retry,
    Quiescent,
}
fn turnover(error: PeerError) -> Result<Reconciled, ControlError> {
    match error {
        PeerError::StaleEpoch | PeerError::Unauthorized => Ok(Reconciled::Retry),
        PeerError::Quiescent => Ok(Reconciled::Quiescent),
        PeerError::Storage => Err(ControlError::Storage),
        PeerError::Transport | PeerError::TimedOut | PeerError::Closed => {
            Err(ControlError::Transport)
        }
        _ => Err(ControlError::Rejected),
    }
}
impl Codes {
    pub async fn reconcile(&self, state: &State) -> Result<Reconciled, ControlError> {
        if state
            .settings
            .code_policy()
            .map_err(|_| ControlError::Invalid)?
            .is_none()
        {
            return Ok(Reconciled::Current);
        }
        let leases = match state.authority.recover_code_intents().await {
            Ok(leases) => leases,
            Err(error) => {
                let result = turnover(error)?;
                if result == Reconciled::Quiescent {
                    self.shutdown().await?;
                }
                return Ok(result);
            }
        };
        self.bind_leases(state, leases).await
    }
    async fn bind_leases(
        &self,
        state: &State,
        leases: Vec<Arc<CodeLease>>,
    ) -> Result<Reconciled, ControlError> {
        let mut entries = self.entries.lock().await;
        let retired = entries
            .iter()
            .filter(|(id, entry)| {
                entry.task.is_finished()
                    || !leases.iter().any(|lease| {
                        lease.id() == **id && lease.generation() == entry.lease.generation()
                    })
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in retired {
            if let Some(entry) = entries.remove(&id) {
                entry.stop.send_replace(true);
                if let Err(error) = entry.task.await.map_err(|_| ControlError::Storage)? {
                    let result = turnover(error)?;
                    if result == Reconciled::Quiescent {
                        drop(entries);
                        self.shutdown().await?;
                        return Ok(result);
                    }
                }
            }
        }
        for lease in leases {
            if entries.contains_key(&lease.id()) {
                continue;
            }
            let config = state.settings.endpoint_config(iroh::SecretKey::generate());
            let relay_only = !config.direct_paths;
            let bind_address = std::net::SocketAddr::new(config.bind_address.ip(), 0);
            let listener = match CodeListener::bind(
                state.authority.clone(),
                lease.clone(),
                CodeTransport {
                    relay: config.relay,
                    bind_address,
                    direct_paths: config.direct_paths,
                    relay_root_certificates: config.relay_root_certificates,
                    timeout: DEADLINE,
                },
            )
            .await
            {
                Ok(listener) => listener,
                Err(error) => {
                    let result = turnover(error)?;
                    drop(entries);
                    if result == Reconciled::Quiescent {
                        self.shutdown().await?;
                    }
                    return Ok(result);
                }
            };
            let publication = tokio::time::timeout(DEADLINE, async {
                loop {
                    let publication = listener.publication().await?;
                    let address = publication.address().clone();
                    drop(publication);
                    if !relay_only || address.relay_urls().next().is_some() {
                        return Ok(address);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            let address = match publication {
                Ok(Ok(address)) => address,
                result => {
                    listener.shutdown().await;
                    let error = match result {
                        Ok(Err(error)) => error,
                        _ => PeerError::TimedOut,
                    };
                    let result = turnover(error)?;
                    drop(entries);
                    if result == Reconciled::Quiescent {
                        self.shutdown().await?;
                    }
                    return Ok(result);
                }
            };
            let (stop, cancel) = watch::channel(false);
            let task = tokio::spawn(async move { listener.serve(cancel).await });
            entries.insert(
                lease.id(),
                Entry {
                    lease,
                    address,
                    stop,
                    task,
                },
            );
        }
        drop(entries);
        let effects = match state.authority.code_effects().await {
            Ok(effects) => effects,
            Err(error) => return self.after_error(error).await,
        };
        for effect in effects {
            if let ControlEffect::RetireCode { id, .. } = &effect
                && !self.entries.lock().await.contains_key(id)
                && let Err(error) = state.authority.acknowledge_code_effect(effect).await
            {
                return self.after_error(error).await;
            }
        }
        Ok(Reconciled::Current)
    }
    async fn after_error(&self, error: PeerError) -> Result<Reconciled, ControlError> {
        let result = turnover(error)?;
        if result == Reconciled::Quiescent {
            self.shutdown().await?;
        }
        Ok(result)
    }
    pub async fn publication(
        &self,
        state: &State,
        id: u64,
    ) -> Result<control::NetworkResponse, ControlError> {
        let leases = state
            .authority
            .recover_code_intents()
            .await
            .map_err(|_| ControlError::Rejected)?;
        let effects = state
            .authority
            .code_effects()
            .await
            .map_err(|_| ControlError::Storage)?;
        let entries = self.entries.lock().await;
        let checked = now().map_err(|_| ControlError::Storage)?;
        let leases = leases
            .into_iter()
            .filter(|lease| {
                entries.get(&lease.id()).is_some_and(|entry| {
                    !entry.task.is_finished() && entry.lease.generation() == lease.generation()
                }) && lease.expires_ms() > checked
            })
            .collect::<Vec<_>>();
        let Some(first) = leases.first() else {
            return control::NetworkResponse::success(
                id,
                serde_json::json!({"kind":"code_publications","publications":[]}),
                None,
            );
        };
        let permit = state
            .authority
            .code_permit(first.clone())
            .await
            .map_err(|_| ControlError::Busy)?;
        let mut values = Vec::new();
        for lease in &leases {
            if lease.generation() != permit.lease().generation() {
                return Err(ControlError::Busy);
            }
            let entry = entries.get(&lease.id()).ok_or(ControlError::Busy)?;
            values.push(serde_json::json!({"intent":lease.id(),"endpoint":entry.address}));
        }
        drop(entries);
        let generation = permit.lease().generation();
        let effects = effects
            .into_iter()
            .filter(|effect| {
                matches!(effect,
            ControlEffect::ReconcileCode { id, generation: g }
            if *g == generation && leases.iter().any(|lease| lease.id() == *id))
            })
            .collect();
        Ok(control::NetworkResponse::success(
            id,
            serde_json::json!({"kind":"code_publications","publications":values}),
            Some(control::PublicationGuard::Permit { _permit: permit }),
        )?
        .acknowledge_after_write(state.authority.clone(), effects))
    }
    pub async fn shutdown(&self) -> Result<(), ControlError> {
        let entries = std::mem::take(&mut *self.entries.lock().await);
        for entry in entries.values() {
            entry.stop.send_replace(true);
        }
        for entry in entries.into_values() {
            if let Err(error) = entry.task.await.map_err(|_| ControlError::Storage)? {
                match turnover(error)? {
                    Reconciled::Retry | Reconciled::Quiescent => {}
                    Reconciled::Current => unreachable!(),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "codes_tests.rs"]
mod tests;
