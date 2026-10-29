use super::*;
use crate::tool_process::ToolProcessSpec;
use serde_json::Value;

impl TrustedToolPolicy {
    pub fn new(
        helper: PathBuf,
        directory: Dir,
        grants: Vec<host::Grant>,
        limits: host::HostLimits,
        timeout: Duration,
    ) -> Result<Self, RuntimeError> {
        if !helper.is_absolute() || timeout.is_zero() {
            return Err(RuntimeError::Invalid(
                "tool helper requires absolute path and positive timeout",
            ));
        }
        let policy = Self {
            identity: ToolPolicyIdentity::new(),
            authorization_required: false,
            helper,
            directory,
            grants,
            limits,
            timeout,
        };
        let sample = host::Request {
            version: host::VERSION,
            attempt_id: "validation".into(),
            call_id: "validation".into(),
            grants: policy.grants.clone(),
            limits: policy.limits.clone(),
            tool: "read".into(),
            input: host::Input::Path(host::PathArgs { path: ".".into() }),
        };
        checked_request(&sample)?;
        Ok(policy)
    }

    /// Opt into mandatory consuming authorization; bare durable claims cannot dispatch.
    pub fn require_authorization(mut self) -> Self {
        self.authorization_required = true;
        self
    }

    pub fn identity(&self) -> ToolPolicyIdentity {
        self.identity.clone()
    }

    pub fn try_clone(&self) -> Result<Self, RuntimeError> {
        Ok(Self {
            identity: self.identity.clone(),
            authorization_required: self.authorization_required,
            helper: self.helper.clone(),
            directory: self.directory.try_clone()?,
            grants: self.grants.clone(),
            limits: self.limits.clone(),
            timeout: self.timeout,
        })
    }

    pub(super) fn apply_grant(
        &self,
        request: &mut host::Request,
        grant: ToolDispatchGrant,
    ) -> Result<(), ToolAdmissionFailure> {
        let required = match (&*request.tool, &request.input) {
            ("read", host::Input::Path(_)) => host::Grant::Read,
            ("list", host::Input::Path(_)) => host::Grant::List,
            ("write", host::Input::Write(args)) => match args.mode {
                host::WriteMode::Create => host::Grant::Create,
                host::WriteMode::Overwrite => host::Grant::Overwrite,
            },
            _ => return Err(ToolAdmissionFailure::Invalid),
        };
        if !grant.grants.contains(&required)
            || grant.grants.iter().any(|item| !self.grants.contains(item))
        {
            return Err(ToolAdmissionFailure::Invalid);
        }
        request.grants = grant.grants;
        checked_request(request).map_err(|_| ToolAdmissionFailure::Invalid)
    }

    pub(super) fn prepare(
        &self,
        attempt: &str,
        call: &str,
        name: &str,
        input: Value,
    ) -> Result<host::Request, &'static str> {
        let input: host::Input = serde_json::from_value(input).map_err(|_| "invalid")?;
        let grant = match (name, &input) {
            ("read", host::Input::Path(_)) => host::Grant::Read,
            ("list", host::Input::Path(_)) => host::Grant::List,
            ("write", host::Input::Write(args)) => match args.mode {
                host::WriteMode::Create => host::Grant::Create,
                host::WriteMode::Overwrite => host::Grant::Overwrite,
            },
            _ => return Err("invalid"),
        };
        if !self.authorization_required && !self.grants.contains(&grant) {
            return Err("denied");
        }
        let request = host::Request {
            version: host::VERSION,
            attempt_id: attempt.into(),
            call_id: call.into(),
            grants: self.grants.clone(),
            limits: self.limits.clone(),
            tool: name.into(),
            input,
        };
        checked_request(&request).map_err(|_| "limit")?;
        Ok(request)
    }

    pub(super) fn spec(&self) -> Result<ToolProcessSpec, RuntimeError> {
        Ok(ToolProcessSpec {
            helper: self.helper.clone(),
            directory: self.directory.try_clone()?,
            timeout: self.timeout,
        })
    }
}

fn checked_request(request: &host::Request) -> Result<(), RuntimeError> {
    let bytes =
        serde_json::to_vec(request).map_err(|_| RuntimeError::Invalid("invalid tool request"))?;
    if bytes.len() > host::MAX_REQUEST_BYTES || bytes.len() > request.limits.request_bytes {
        return Err(RuntimeError::Invalid(
            "tool request exceeds configured bound",
        ));
    }
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    host::read_request(&mut std::io::Cursor::new(frame))
        .map_err(|_| RuntimeError::Invalid("invalid tool policy or request"))?;
    Ok(())
}
