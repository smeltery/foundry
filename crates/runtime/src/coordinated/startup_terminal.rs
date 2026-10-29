//! Terminal owner phases share the startup controller and its sole reply receiver.
use super::*;
use crate::coordinated::capture::*;
use foundry_contracts::{Lifecycle, Outcome};

#[derive(PartialEq, Eq)]
enum TerminalPhase {
    Freezing,
    Draining,
    Cutting,
    Candidate,
    Validating,
    Verified,
}
pub(super) struct Terminal {
    phase: TerminalPhase,
    committed: bool,
    proposal: Option<CaptureTerminal>,
    cut: Option<CaptureCut>,
    expected: Option<ContextDigest>,
    verified: Option<VerifiedCapture>,
}
impl Startup {
    pub fn begin_terminal(&mut self, outcome: Outcome, result: &str) -> Result<(), RuntimeError> {
        if !self.active() || self.terminal.is_some() || !self.trace.is_quiescent() {
            return Err(protocol("capture terminal is not quiescent"));
        }
        let proposal = CaptureTerminal::new(Lifecycle::Exit, outcome, result)
            .map_err(|_| protocol("invalid capture terminal proposal"))?;
        self.correlation.job = CaptureJobId::fresh();
        self.terminal = Some(Terminal {
            phase: TerminalPhase::Freezing,
            committed: false,
            proposal: Some(proposal),
            cut: None,
            expected: None,
            verified: None,
        });
        self.pending = Some(CaptureRequest::FreezeAdmission {
            correlation: self.correlation.clone(),
        });
        self.since = Instant::now();
        Ok(())
    }
    pub fn terminal_pending(&self) -> bool {
        self.terminal.is_some()
    }
    pub fn terminal_committed(&mut self) -> Result<(), RuntimeError> {
        let terminal = self
            .terminal
            .as_mut()
            .ok_or_else(|| protocol("unexpected terminal receipt"))?;
        if terminal.committed {
            return Err(protocol("duplicate terminal receipt"));
        }
        terminal.committed = true;
        Ok(())
    }
    pub fn take_verified(&mut self) -> Option<VerifiedCapture> {
        self.terminal.as_mut()?.verified.take()
    }
    pub fn check_owner_drained(&self) -> Result<(), RuntimeError> {
        if self.since.elapsed() >= self.plan.timeout {
            return Err(protocol("capture terminal deadline exceeded"));
        }
        match self.owner.replies.try_recv() {
            Err(TryRecvError::Empty) => Ok(()),
            _ => Err(protocol("late or disconnected capture owner reply")),
        }
    }
    pub fn advance_cut(&mut self) -> Result<(), RuntimeError> {
        let terminal = self
            .terminal
            .as_mut()
            .ok_or_else(|| protocol("capture terminal missing"))?;
        if terminal.phase == TerminalPhase::Draining && terminal.committed {
            if !self.trace.is_quiescent() {
                return Err(protocol("capture trace is unresolved"));
            }
            self.correlation.job = CaptureJobId::fresh();
            self.pending = Some(CaptureRequest::PrepareCut {
                correlation: self.correlation.clone(),
            });
            terminal.phase = TerminalPhase::Cutting;
            self.since = Instant::now();
        }
        Ok(())
    }
    pub fn terminal_context(&mut self, event: ContextEvent) -> Result<(), RuntimeError> {
        let terminal = self
            .terminal
            .as_mut()
            .ok_or_else(|| protocol("unexpected context"))?;
        if terminal.phase != TerminalPhase::Candidate {
            return Err(protocol("unexpected terminal context phase"));
        }
        self.correlation.job = CaptureJobId::fresh();
        let job = CaptureValidationJob::with_origin(
            &self.correlation,
            terminal
                .cut
                .take()
                .ok_or_else(|| protocol("missing capture cut"))?,
            event,
            &self.challenge,
            std::mem::take(&mut self.trace),
            terminal
                .proposal
                .take()
                .ok_or_else(|| protocol("missing terminal proposal"))?,
            self.restored_origin.clone(),
        )
        .map_err(|_| protocol("capture candidate binding rejected"))?;
        terminal.expected = Some(job.binding_digest());
        self.pending = Some(CaptureRequest::ValidateCandidate {
            correlation: self.correlation.clone(),
            job: Box::new(job),
        });
        terminal.phase = TerminalPhase::Validating;
        self.since = Instant::now();
        Ok(())
    }
    pub fn poll_terminal(
        &mut self,
        process: &Process,
        session: &Session,
    ) -> Result<(), RuntimeError> {
        if self.since.elapsed() >= self.plan.timeout {
            return Err(protocol("capture terminal deadline exceeded"));
        }
        let reply = match self.owner.replies.try_recv() {
            Ok(reply) => reply,
            Err(TryRecvError::Empty) => return self.submit_activation(),
            Err(TryRecvError::Disconnected) => {
                return Err(protocol("capture owner reply channel closed"));
            }
        };
        if self.pending.is_some() {
            return Err(protocol("capture reply preceded request submission"));
        }
        let terminal = self
            .terminal
            .as_mut()
            .ok_or_else(|| protocol("missing terminal phase"))?;
        match reply {
            CaptureReply::AdmissionFrozen { correlation }
                if correlation == self.correlation && terminal.phase == TerminalPhase::Freezing =>
            {
                terminal.phase = TerminalPhase::Draining;
            }
            CaptureReply::CutPrepared { correlation, cut }
                if correlation == self.correlation && terminal.phase == TerminalPhase::Cutting =>
            {
                if let Some(origin) = &self.restored_origin {
                    origin
                        .check_cut(&cut)
                        .map_err(|_| protocol("restored capture parent mismatch"))?;
                } else if cut.identity().expected_head.is_some() {
                    return Err(protocol("restored capture baseline is unsupported"));
                }
                if cut.identity().session_id != session.id
                    || cut.identity().attempt_id != self.correlation.attempt_id.as_str()
                    || !same_profile(cut.profile(), &self.plan.profile)
                {
                    return Err(protocol("prepared cut binding mismatch"));
                }
                self.challenge = ContextReceipt::new(uuid::Uuid::new_v4().to_string())
                    .map_err(|_| protocol("export challenge unavailable"))?;
                process.send_context(&ContextCommand::Export {
                    attempt_id: self.correlation.attempt_id.clone(),
                    capture_id: self.correlation.capture_id.clone(),
                    format_digest: self.format_digest,
                    cut_digest: cut.binding_digest(),
                    challenge: self.challenge.clone(),
                })?;
                terminal.cut = Some(*cut);
                terminal.phase = TerminalPhase::Candidate;
                self.since = Instant::now();
            }
            CaptureReply::CandidateVerified {
                correlation,
                verified,
            } if correlation == self.correlation && terminal.phase == TerminalPhase::Validating => {
                if Some(verified.binding_digest()) != terminal.expected {
                    return Err(protocol("verified capture binding mismatch"));
                }
                terminal.verified = Some(*verified);
                terminal.phase = TerminalPhase::Verified;
            }
            _ => return Err(protocol("unexpected terminal owner reply")),
        }
        self.check_owner_drained()
    }
}
fn same_profile(a: &ProfileBinding, b: &ProfileBinding) -> bool {
    a.configuration_digest == b.configuration_digest
        && a.stored.format_id == b.stored.format_id
        && a.stored.format_version == b.stored.format_version
        && a.stored.profile_id == b.stored.profile_id
        && a.stored.binding_id == b.stored.binding_id
        && a.stored.binding_version == b.stored.binding_version
        && a.wire.profile == b.wire.profile
        && a.wire.format_version == b.wire.format_version
        && a.wire.producer_build == b.wire.producer_build
}
