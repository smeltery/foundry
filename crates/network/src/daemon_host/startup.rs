use super::*;
use crate::daemon_config::NetworkMode;
pub(super) async fn open(
    config: DaemonConfig,
    reference_base: Option<&Path>,
) -> Result<Daemon, HostError> {
    config.validate().map_err(|_| HostError::Configuration)?;
    let settings = match &config.network {
        NetworkMode::Disabled => None,
        NetworkMode::Enabled(network) => {
            let base = reference_base
                .filter(|p| p.is_absolute())
                .ok_or(HostError::Configuration)?;
            Some(
                network
                    .resolve(base)
                    .map_err(|_| HostError::Configuration)?,
            )
        }
    };
    let policy = config
        .folder_policy()
        .map_err(|_| HostError::Configuration)?;
    let root = config
        .state_directory
        .canonicalize()
        .map_err(|_| HostError::Startup)?;
    let directory =
        Dir::open_ambient_dir(&root, ambient_authority()).map_err(|_| HostError::Startup)?;
    let lease = DaemonLease::acquire(&directory).map_err(|_| HostError::Startup)?;
    let engine_lease = lease.retain().map_err(|_| HostError::Startup)?;
    let engine_root = root.clone();
    let identity = TrustedIdentity {
        person: config.person.clone(),
        machine: config.machine.clone(),
    };
    let engine = tokio::task::spawn_blocking(move || {
        let engine = Engine::open(&engine_root, policy, identity)?;
        Ok::<_, EngineError>(OwnedEngine {
            inner: Mutex::new(engine),
            replies: Arc::new(Semaphore::new(1)),
            root: directory,
            _lease: engine_lease,
        })
    })
    .await
    .map_err(|_| HostError::Task)?
    .map_err(|_| HostError::Storage)?;
    let engine = Arc::new(engine);
    let (network, network_owner) = match settings {
        None => (None, None),
        Some(settings) => {
            let (handle, owner) = crate::daemon_network::start(
                root.clone(),
                network::EngineAccess::new(engine.clone()),
                settings,
                config.person,
                config.machine,
            )
            .await
            .map_err(|_| HostError::Startup)?;
            (Some(handle), Some(owner))
        }
    };
    let listener = LocalListener::bind_at(&root, &config.socket_directory, lease, WRITE_TIMEOUT)
        .await
        .map_err(|_| HostError::Startup)?;
    Ok(Daemon {
        listener,
        engine,
        cancellation: Arc::new(WaitRegistry::default()),
        network,
        network_owner,
    })
}
