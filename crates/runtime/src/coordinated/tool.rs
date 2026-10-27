use super::*;
use crate::{
    process::Process,
    tool_process::{ToolPoll, ToolProcess, ToolProcessError},
};
use foundry_contracts::AdapterCommand;
use std::sync::{Arc, atomic::AtomicBool};

#[derive(PartialEq, Eq)]
enum Phase {
    Claim,
    Running,
    Finish,
    Ready,
}

pub(super) struct Job {
    pub checkpoint: CheckpointId,
    pub pending: Option<PersistenceRequest>,
    phase: Phase,
    pub helper: Option<ToolProcess>,
    request: Option<host::Request>,
    call_id: String,
    disposition: Option<ToolDisposition>,
    result: Option<serde_json::Value>,
    cleanup_failed: bool,
    evidence_required: bool,
    expects_digest: bool,
    ledger_digest: Option<[u8; 32]>,
    shared_reply: Option<SharedToolReply>,
}
impl Job {
    pub fn new(request: host::Request) -> Self {
        Self::with_evidence(request, false)
    }
    pub fn new_capture(request: host::Request) -> Self {
        Self::with_evidence(request, true)
    }
    fn with_evidence(request: host::Request, evidence_required: bool) -> Self {
        let checkpoint = CheckpointId::new();
        let intent = ToolIntent {
            call_id: request.call_id.clone(),
            tool: request.tool.clone(),
            input: request.input.clone(),
        };
        Self {
            checkpoint,
            pending: Some(PersistenceRequest::ClaimTool { checkpoint, intent }),
            phase: Phase::Claim,
            helper: None,
            call_id: request.call_id.clone(),
            request: Some(request),
            disposition: None,
            result: None,
            cleanup_failed: false,
            evidence_required,
            expects_digest: false,
            ledger_digest: None,
            shared_reply: None,
        }
    }

    pub fn reply(
        &mut self,
        reply: PersistenceReply,
        policy: &TrustedToolPolicy,
        stop: Arc<AtomicBool>,
    ) -> Result<(), RuntimeError> {
        if self.pending.is_some() {
            return Err(protocol("tool reply preceded request submission"));
        }
        match reply {
            PersistenceReply::ToolClaimed { checkpoint }
                if checkpoint == self.checkpoint && self.phase == Phase::Claim =>
            {
                let request = self
                    .request
                    .take()
                    .ok_or_else(|| protocol("missing owned tool request"))?;
                match policy.spec().and_then(|spec| ToolProcess::start(spec, request, stop).map_err(|_| protocol("tool failed before lifecycle start"))) {
                    Ok(helper) => { self.helper = Some(helper); self.phase = Phase::Running; }
                    Err(_) => self.finish(ToolEvidence::FailedBeforeSpawn, ToolDisposition::NoEffectFailure,
                        Some(serde_json::json!({"status":"error","code":"tool_start_failed","effect":"none"}))),
                }
            }
            PersistenceReply::ToolFinished {
                checkpoint,
                disposition,
            } => {
                self.accept_finished(checkpoint, disposition, None, false)?;
            }
            PersistenceReply::ToolFinishedEvidence {
                checkpoint,
                disposition,
                output_digest,
            } => {
                self.accept_finished(checkpoint, disposition, output_digest, true)?;
            }
            _ => return Err(protocol("unexpected tool persistence reply")),
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), RuntimeError> {
        let Some(helper) = self.helper.as_mut() else {
            return Ok(());
        };
        match helper.poll() {
            ToolPoll::Pending => {}
            ToolPoll::Consumed => return Err(protocol("tool completion consumed twice")),
            ToolPoll::Finished(result) => {
                self.helper.take();
                match result {
                    Ok(reply) => {
                        let disposition = match &reply.result {
                            host::Outcome::Ok { .. } => ToolDisposition::Success,
                            host::Outcome::Error {
                                effect: host::Effect::None,
                                ..
                            } => ToolDisposition::NoEffectFailure,
                            host::Outcome::Error {
                                effect: host::Effect::Unknown,
                                ..
                            } => ToolDisposition::Uncertain,
                        };
                        if self.evidence_required {
                            let shared = SharedToolReply::new(reply);
                            self.shared_reply = Some(shared.clone());
                            self.finish(ToolEvidence::SharedReply(shared), disposition, None);
                        } else {
                            let result = serde_json::to_value(&reply.result)
                                .map_err(|_| protocol("tool result encoding failed"))?;
                            self.finish(ToolEvidence::Replied(reply), disposition, Some(result));
                        }
                    }
                    Err(error) => {
                        self.cleanup_failed =
                            matches!(error, ToolProcessError::Cleanup | ToolProcessError::Worker);
                        self.finish(ToolEvidence::Uncertain, ToolDisposition::Uncertain, None);
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(
        &mut self,
        evidence: ToolEvidence,
        disposition: ToolDisposition,
        result: Option<serde_json::Value>,
    ) {
        self.phase = Phase::Finish;
        self.disposition = Some(disposition);
        self.result = result;
        self.expects_digest = matches!(
            evidence,
            ToolEvidence::SharedReply(_) | ToolEvidence::Replied(_)
        );
        self.pending = Some(if self.evidence_required {
            PersistenceRequest::FinishToolWithEvidence {
                checkpoint: self.checkpoint,
                evidence,
            }
        } else {
            PersistenceRequest::FinishTool {
                checkpoint: self.checkpoint,
                evidence,
            }
        });
    }

    fn accept_finished(
        &mut self,
        checkpoint: CheckpointId,
        disposition: ToolDisposition,
        digest: Option<[u8; 32]>,
        evidence: bool,
    ) -> Result<(), RuntimeError> {
        if self.pending.is_some()
            || checkpoint != self.checkpoint
            || self.phase != Phase::Finish
            || evidence != self.evidence_required
            || self.disposition != Some(disposition)
            || (evidence && digest.is_some() != self.expects_digest)
        {
            return Err(protocol("tool receipt evidence mismatch"));
        }
        self.ledger_digest = digest;
        self.phase = Phase::Ready;
        Ok(())
    }

    pub fn deliver(
        &mut self,
        process: &Process,
        attempt: &str,
        trace: Option<&mut capture::DeliveredTrace>,
    ) -> Result<bool, RuntimeError> {
        if self.phase != Phase::Ready {
            return Ok(false);
        }
        if self.disposition == Some(ToolDisposition::Uncertain) {
            return Err(protocol("tool effects are uncertain"));
        }
        if self.evidence_required {
            let trace = trace.ok_or_else(|| protocol("capture trace missing"))?;
            let reservation = if let Some(reply) = &self.shared_reply {
                trace.reserve_helper_result(
                    attempt,
                    &self.call_id,
                    self.ledger_digest
                        .ok_or_else(|| protocol("missing ledger digest"))?,
                    &reply.reply().result,
                )
            } else {
                trace.reserve_result(
                    attempt,
                    &self.call_id,
                    capture::ResultProvenance::SyntheticNoEffect,
                    self.result
                        .as_ref()
                        .ok_or_else(|| protocol("missing synthetic result"))?,
                )
            }
            .map_err(|_| protocol("tool result trace rejected"))?;
            process.send_reserved_result(attempt, &self.call_id, &reservation)?;
            reservation.commit_enqueued();
            return Ok(true);
        }
        process.send(&AdapterCommand::ToolResult {
            attempt_id: attempt.into(),
            call_id: self.call_id.clone(),
            result: self
                .result
                .take()
                .ok_or_else(|| protocol("missing durable tool result"))?,
        })?;
        Ok(true)
    }

    pub fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if let Some(helper) = self.helper.as_mut() {
            helper.request_stop();
            loop {
                match helper.poll() {
                    ToolPoll::Pending => std::thread::sleep(Duration::from_millis(10)),
                    ToolPoll::Finished(Err(
                        ToolProcessError::Cleanup | ToolProcessError::Worker,
                    )) => {
                        self.helper.take();
                        return Err(protocol("tool cleanup failed"));
                    }
                    ToolPoll::Finished(_) | ToolPoll::Consumed => break,
                }
            }
            self.helper.take();
        }
        if self.cleanup_failed {
            return Err(protocol("tool cleanup failed"));
        }
        Ok(())
    }
}
pub(super) fn protocol(message: &'static str) -> RuntimeError {
    RuntimeError::Protocol(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_before_lifecycle_start_requests_explicit_evidence_with_no_digest() {
        let policy = TrustedToolPolicy::new(
            std::env::temp_dir().join("unused-helper"),
            cap_std::fs::Dir::open_ambient_dir(std::env::temp_dir(), cap_std::ambient_authority())
                .unwrap(),
            vec![host::Grant::Read],
            host::HostLimits {
                request_bytes: host::MAX_REQUEST_BYTES,
                read_bytes: 262144,
                write_bytes: 262144,
                output_bytes: host::MAX_REPLY_BYTES,
                entries: 256,
            },
            Duration::from_secs(1),
        )
        .unwrap();
        let request = policy
            .prepare(
                "attempt",
                "call",
                "read",
                serde_json::json!({"path":"file"}),
            )
            .unwrap();
        let mut job = Job::new_capture(request);
        job.pending.take();
        job.reply(
            PersistenceReply::ToolClaimed {
                checkpoint: job.checkpoint,
            },
            &policy,
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap();
        assert!(matches!(
            job.pending.take(),
            Some(PersistenceRequest::FinishToolWithEvidence {
                evidence: ToolEvidence::FailedBeforeSpawn,
                ..
            })
        ));
        assert!(
            job.accept_finished(
                job.checkpoint,
                ToolDisposition::NoEffectFailure,
                Some([1; 32]),
                true
            )
            .is_err()
        );
        job.accept_finished(job.checkpoint, ToolDisposition::NoEffectFailure, None, true)
            .unwrap();
        assert!(job.phase == Phase::Ready);
    }
}
