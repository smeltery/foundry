use super::*;
impl Actor {
    pub(super) fn job(
        &self,
        store: &mut Store,
        reservation: &mut Reservation,
        kind: JobKind,
    ) -> Result<RestoreReply> {
        match kind {
            JobKind::OpenCapture(configuration) => {
                if reservation.publication.is_some() {
                    return Err(ServiceError::Coordination("capture already opened"));
                }
                let grant = reservation
                    .continuation
                    .as_ref()
                    .and_then(|link| link.grant.as_ref())
                    .ok_or(ServiceError::Coordination("capture attempt absent"))?;
                let acceptance = grant
                    .acceptance
                    .clone()
                    .ok_or(ServiceError::Coordination("capture acceptance absent"))?;
                let validated = reservation
                    .native
                    .as_ref()
                    .and_then(|n| n.validated.as_ref())
                    .ok_or(ServiceError::Coordination("native validation absent"))?;
                if configuration.profile.configuration_wire_digest().ok()
                    != Some(validated.effective_configuration_digest)
                {
                    return Err(ServiceError::Coordination(
                        "capture effective policy mismatch",
                    ));
                }
                let control = crate::tool_worker::WorkerControl {
                    signals: self.io.signals.clone(),
                    activate_on_ready: false,
                    expired: Arc::clone(&self.io.expired),
                    cancellation: self.io.cancel.clone(),
                };
                let (publication, ports) = crate::publication::Publication::new(
                    store,
                    &grant.fence,
                    *configuration,
                    control,
                    reservation.snapshot.clone(),
                    Some(acceptance),
                )?;
                reservation.publication = Some(publication);
                Ok(RestoreReply::CaptureReady(Box::new(ports)))
            }
            JobKind::FinishCapture(prepared) => {
                let grant = reservation
                    .continuation
                    .as_ref()
                    .and_then(|link| link.grant.as_ref())
                    .ok_or(ServiceError::Coordination("capture attempt absent"))?;
                let publication = reservation
                    .publication
                    .as_mut()
                    .ok_or(ServiceError::Coordination("capture owner absent"))?;
                let result = publication.finish(store, &grant.fence, *prepared)?;
                reservation.published = true;
                Ok(RestoreReply::Published(Box::new(result)))
            }
            JobKind::OpenContinuation(selection) => {
                if reservation.continuation.is_some()
                    || reservation
                        .native
                        .as_ref()
                        .and_then(|n| n.validated.as_ref())
                        .is_none()
                {
                    return Err(ServiceError::Coordination("continuation unavailable"));
                }
                let (link, owner) = super::super::continuation::Link::new(selection)?;
                reservation.continuation = Some(link);
                Ok(RestoreReply::Continuation(owner))
            }
            JobKind::PrepareNative(registration, expected) => {
                if reservation.native.is_some() {
                    return Err(ServiceError::Coordination(
                        "native registration already fixed",
                    ));
                }
                let begin = reservation
                    .binding
                    .ok_or(ServiceError::Coordination("restore plan unbound"))?;
                let native = NativeState::prepare(
                    registration,
                    expected,
                    reservation.scope.clone(),
                    begin,
                    reservation.snapshot.as_ref().expect("selected snapshot"),
                )?;
                let reply = RestoreReply::NativePrepared(native.binding.clone());
                reservation.native = Some(native);
                Ok(reply)
            }
            JobKind::ValidateNative {
                frame,
                instance,
                scope,
                begin,
            } => {
                if scope != reservation.scope
                    || reservation.binding != Some(begin)
                    || reservation.cleanup.as_ref().map(CleanupObserver::status)
                        != Some(CleanupStatus::Pending)
                {
                    return Err(ServiceError::Coordination("native runtime owner mismatch"));
                }
                reservation
                    .native
                    .as_mut()
                    .ok_or(ServiceError::Coordination("native comparator absent"))?
                    .start(
                        Arc::clone(reservation.snapshot.as_ref().expect("selected snapshot")),
                        frame,
                        instance,
                    )?;
                Ok(RestoreReply::NativeStarted)
            }
            JobKind::NativeStatus => {
                let native = reservation
                    .native
                    .as_ref()
                    .ok_or(ServiceError::Coordination("native comparator absent"))?;
                Ok(match &native.validated {
                    Some(value) => RestoreReply::NativeValidated(super::super::validation::digest(
                        value.validation_digest,
                    )),
                    None => RestoreReply::NativePending,
                })
            }
            JobKind::Baseline => {
                if reservation.exposed {
                    return Err(ServiceError::Coordination(
                        "restore baseline already handed off",
                    ));
                }
                // Revalidate current selection at transfer, not merely at reservation.
                let snapshot = Arc::new(store.restore_baseline_snapshot(&reservation.fence)?);
                self.live(store, &reservation.session)?;
                reservation.snapshot = Some(Arc::clone(&snapshot));
                reservation.exposed = true;
                Ok(RestoreReply::Baseline(snapshot))
            }
            JobKind::BindPlan(descriptor) => {
                if !reservation.exposed || reservation.binding.is_some() {
                    return Err(ServiceError::Coordination(
                        "restore plan already bound or unavailable",
                    ));
                }
                let snapshot = reservation.snapshot.as_ref().expect("selected snapshot");
                let head = snapshot.checkpoint().reference();
                let digest =
                    |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
                if descriptor.checkpoint_id.as_str() != head.id.as_str()
                    || descriptor.record_digest.to_hex() != digest(&head.record_digest)
                    || descriptor.witness_digest.to_hex() != digest(&snapshot.witness().digest())
                    || descriptor.configuration_digest.to_hex()
                        != digest(&head.configuration_digest)
                    || descriptor.payload_digest.to_hex() != digest(&head.payload_digest)
                    || descriptor.payload_length != head.payload_length
                {
                    return Err(ServiceError::Coordination(
                        "restore plan differs from selected baseline",
                    ));
                }
                reservation.binding = Some(
                    foundry_contracts::context_v2::restore_binding(&reservation.scope, &descriptor)
                        .map_err(|_| ServiceError::Coordination("invalid restore plan"))?,
                );
                Ok(RestoreReply::PlanBound(descriptor))
            }
            JobKind::RegisterCleanup {
                scope,
                binding,
                observer,
            } => {
                if !reservation.exposed
                    || scope != reservation.scope
                    || reservation.binding != Some(binding)
                    || reservation.cleanup.is_some()
                {
                    return Err(ServiceError::Coordination("restore cleanup owner mismatch"));
                }
                reservation.cleanup = Some(observer);
                Ok(RestoreReply::CleanupRegistered)
            }
        }
    }
}
