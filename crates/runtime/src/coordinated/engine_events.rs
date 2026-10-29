//! Persistence acknowledgments and ordinary event admission for the single coordinator.
use super::*;

impl Coordinator<'_> {
    pub(super) fn persisted(
        &mut self,
        reply: PersistenceReply,
    ) -> Result<Option<Proposal>, RuntimeError> {
        match reply {
            PersistenceReply::Rejected { .. } => {
                return Err(protocol("persistence rejected checkpoint"));
            }
            PersistenceReply::EventCommitted { checkpoint } => {
                let job = self
                    .event
                    .take()
                    .ok_or_else(|| protocol("unexpected event acknowledgment"))?;
                if job.checkpoint != checkpoint || job.pending.is_some() {
                    return Err(protocol("event checkpoint mismatch"));
                }
                match job.action {
                    Action::None => {}
                    Action::CaptureTerminal => self
                        .startup
                        .as_mut()
                        .ok_or_else(|| protocol("missing capture terminal"))?
                        .terminal_committed()?,
                    Action::MessageReceipt(id) => {
                        self.startup
                            .as_mut()
                            .map(|s| &mut s.trace)
                            .or(self.restored.as_mut())
                            .ok_or_else(|| protocol("receipt outside traced execution"))?
                            .confirm_instruction_persisted(self.attempt, &id, checkpoint)
                            .map_err(|_| protocol("instruction persistence mismatch"))?;
                    }
                    Action::Ready(enabled) => {
                        if let Some(startup) = self.startup.as_mut() {
                            startup.persisted_ready()?;
                            return Ok(None);
                        }
                        self.delivery.enabled = enabled;
                        self.process.send(&AdapterCommand::Start {
                            version: PROTOCOL_VERSION,
                            attempt_id: self.attempt.into(),
                            session: self.session.clone(),
                        })?;
                    }
                    Action::Tool(request) => {
                        self.job = Some(if self.startup.is_some() || self.restored.is_some() {
                            Job::new_capture(request)
                        } else {
                            Job::new(request)
                        })
                    }
                    Action::Denied(call_id, code) => {
                        self.deliver_denied(call_id, code)?;
                    }
                    Action::Terminal(result) => return Ok(Some(result.into())),
                    Action::Failed => return Err(RuntimeError::AdapterFailed),
                }
            }
            reply => {
                let job = self
                    .job
                    .as_mut()
                    .ok_or_else(|| protocol("unexpected tool acknowledgment"))?;
                let policy = self
                    .policy
                    .as_ref()
                    .ok_or_else(|| protocol("tool policy unavailable"))?;
                if let Some(disposition) = job.reply(reply, policy, Arc::clone(&self.stop))? {
                    let call_id = job.call_id().to_owned();
                    self.job.take();
                    match disposition {
                        PermissionDisposition::Denied => self.deliver_denied(call_id, "denied")?,
                        PermissionDisposition::ActionRequired => {
                            self.delivery.enabled = false;
                            return Ok(Some(Proposal::PermissionStop(disposition)));
                        }
                        PermissionDisposition::Unsupported => {
                            self.delivery.enabled = false;
                            return Ok(Some(Proposal::PermissionStop(disposition)));
                        }
                        _ => return Err(protocol("invalid blocked tool disposition")),
                    }
                }
            }
        }
        Ok(None)
    }

    fn deliver_denied(&mut self, call_id: String, code: &'static str) -> Result<(), RuntimeError> {
        let result = serde_json::json!({"status":"denied","code":code});
        if let Some(trace) = self
            .startup
            .as_mut()
            .map(|s| &mut s.trace)
            .or(self.restored.as_mut())
        {
            let reservation = trace
                .reserve_result(
                    self.attempt,
                    &call_id,
                    super::super::capture::ResultProvenance::Denied,
                    &result,
                )
                .map_err(|_| protocol("denial trace rejected"))?;
            self.process
                .send_reserved_result(self.attempt, &call_id, &reservation)?;
            reservation.commit_enqueued();
            return Ok(());
        }
        self.process.send(&AdapterCommand::ToolResult {
            attempt_id: self.attempt.into(),
            call_id,
            result,
        })
    }

    pub(super) fn event(&mut self, mut event: AdapterEvent) -> Result<(), RuntimeError> {
        if self.startup.is_none() {
            self.ingress.require_execution_support()?;
        }
        let checkpoint = CheckpointId::new();
        let action = if let AdapterEvent::Ready {
            version,
            capabilities,
        } = &event
        {
            if self.ready || *version != PROTOCOL_VERSION {
                return Err(protocol("duplicate ready or unsupported version"));
            }
            if let Some(startup) = self.startup.as_mut() {
                startup.ready(capabilities)?;
            }
            self.ready = true;
            Action::Ready(
                capabilities
                    .iter()
                    .any(|value| value == MESSAGE_ACK_CAPABILITY),
            )
        } else {
            if let Some(startup) = self.startup.as_ref() {
                if !startup.active() {
                    return Err(protocol("ordinary event before capture startup"));
                }
                if startup.terminal_pending() {
                    return Err(protocol("ordinary event after capture terminal"));
                }
            }
            let attempt = match &event {
                AdapterEvent::Text { attempt_id, .. }
                | AdapterEvent::ToolCall { attempt_id, .. }
                | AdapterEvent::Settled { attempt_id, .. }
                | AdapterEvent::Failed { attempt_id, .. }
                | AdapterEvent::MessageAccepted { attempt_id, .. } => attempt_id,
                AdapterEvent::Ready { .. } => unreachable!(),
            };
            if !self.ready || attempt != self.attempt {
                return Err(protocol("event belongs to an unready or different attempt"));
            }
            match &event {
                AdapterEvent::MessageAccepted { message_id, .. } => {
                    self.delivery.validate_ack(message_id)?;
                    if let Some(trace) = self
                        .startup
                        .as_mut()
                        .map(|s| &mut s.trace)
                        .or(self.restored.as_mut())
                    {
                        trace
                            .observe_instruction_receipt(self.attempt, message_id, checkpoint)
                            .map_err(|_| protocol("instruction receipt mismatch"))?;
                        Action::MessageReceipt(message_id.clone())
                    } else {
                        Action::None
                    }
                }
                AdapterEvent::ToolCall {
                    call_id,
                    name,
                    input,
                    ..
                } => {
                    if self.job.is_some() {
                        return Err(protocol("concurrent tool calls are not allowed"));
                    }
                    if call_id.trim().is_empty()
                        || call_id.len() > 128
                        || call_id.chars().any(char::is_control)
                        || self.seen.len() >= MAX_TOOL_CALLS
                        || !self.seen.insert(call_id.clone())
                    {
                        return Err(protocol("invalid, repeated or excessive tool call ID"));
                    }
                    match &self.policy {
                        None => Action::Denied(call_id.clone(), "tool_execution_disabled"),
                        Some(policy) => {
                            match policy.prepare(self.attempt, call_id, name, input.clone()) {
                                Ok(request) => Action::Tool(request),
                                Err(code) => Action::Denied(call_id.clone(), code),
                            }
                        }
                    }
                }
                AdapterEvent::Settled {
                    outcome, result, ..
                } => {
                    if self.job.is_some() {
                        return Err(protocol("adapter settled with outstanding tool work"));
                    }
                    if let Some(startup) = self.startup.as_mut() {
                        startup.begin_terminal(*outcome, result)?;
                        self.delivery.enabled = false;
                        Action::CaptureTerminal
                    } else {
                        Action::Terminal(RunResult::Completed {
                            outcome: *outcome,
                            result: result.clone(),
                        })
                    }
                }
                AdapterEvent::Failed { .. } => {
                    self.stop.store(true, Ordering::Release);
                    if let Some(helper) = self.job.as_mut().and_then(|job| job.helper.as_mut()) {
                        helper.request_stop();
                    }
                    event = AdapterEvent::Failed {
                        attempt_id: self.attempt.into(),
                        code: "adapter_failed".into(),
                        message: "Adapter reported failure; details withheld".into(),
                    };
                    Action::Failed
                }
                _ => Action::None,
            }
        };
        if matches!(action, Action::CaptureTerminal)
            && let AdapterEvent::Settled { result, .. } = &mut event
        {
            result.clear();
        }
        self.event = Some(EventJob {
            checkpoint,
            pending: Some(PersistenceRequest::Event { checkpoint, event }),
            action,
        });
        Ok(())
    }
}
