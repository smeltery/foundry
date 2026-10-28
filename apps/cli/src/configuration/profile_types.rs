use super::PendingCause;

pub struct ProfileFacts {
    pub lifecycle: ProfileLifecycle,
    pub kind: ProfileKind,
    pub platform: ProfilePlatform,
    pub home: Option<String>,
    pub cwd: Option<String>,
    pub hostname: Option<String>,
    pub authentication: ProfileAuthentication,
}
pub enum ProfileLifecycle {
    Fresh,
    Retained(SavedProfile),
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    Personal,
    Machine,
    Unknown,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfilePlatform {
    MacOs,
    Other,
    Unknown,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfileAuthentication {
    WithoutKey,
    Unknown,
}
#[derive(Default)]
pub struct SavedProfile {
    pub name: Option<String>,
    pub folder: Option<String>,
    pub proxy_ca_file: Option<String>,
    pub user_chrome: Option<bool>,
    pub local_logins: Option<bool>,
    pub keep_awake: Option<bool>,
    pub minimal: Option<bool>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileOrigin {
    Explicit,
    Retained,
    Hostname,
    Home,
    Cwd,
    PlatformDefault,
    AuthenticationDefault,
}
pub struct ProfileCandidate<T> {
    pub value: T,
    pub origin: ProfileOrigin,
}
pub enum ProfileValue<T> {
    Selected(ProfileCandidate<T>),
    Pending {
        cause: PendingCause,
        candidates: Vec<ProfileCandidate<T>>,
    },
}
pub struct ConnectProfile {
    pub name: ProfileValue<String>,
    pub folder: ProfileValue<String>,
    pub proxy_ca_file: ProfileValue<String>,
    pub user_chrome: ProfileValue<bool>,
    pub local_logins: ProfileValue<bool>,
    pub keep_awake: ProfileValue<bool>,
    pub minimal: ProfileValue<bool>,
    pub changes: ProfileChanges,
}
#[derive(Default)]
pub struct ProfileChanges {
    pub name: Option<String>,
    pub folder: Option<String>,
    pub proxy_ca_file: Option<String>,
    pub user_chrome: Option<bool>,
    pub local_logins: Option<bool>,
    pub keep_awake: Option<bool>,
    pub minimal: bool,
    pub add_folder: Vec<String>,
    pub remove_folder: Vec<String>,
}
impl ProfileChanges {
    /// Requested application timing, not proof of platform execution or support.
    pub fn keep_awake_without_reconnect(&self) -> bool {
        self.keep_awake.is_some()
    }
    pub fn folders_apply_immediately(&self) -> bool {
        !self.add_folder.is_empty() || !self.remove_folder.is_empty()
    }
    pub fn preserves_other_folder_sessions(&self) -> bool {
        true
    }
    pub fn preserves_installed_desktop_profile(&self) -> bool {
        true
    }
}
