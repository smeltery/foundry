//! Workspace settings namespaces. Each namespace belongs to the feature that
//! owns it, which supplies its sensitivity, writability and validation through
//! a [`SettingsCatalog`]; the writable list is unknown in the specification.
//! Values are the complete serialized value object; a replace never merges.
//! The decision core denies `settings.list` and `settings.get` for every role
//! until their matrix cells are established.
use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};

use super::error::{OrgError, Storage};
use super::{Authorized, Caller, Change, OrgStore};
use crate::authz::{OperationRequest, WorkspaceId};

#[derive(Debug, Clone, Copy)]
pub struct SettingsNamespace {
    pub name: &'static str,
    /// Never listed, and `not_found` when read.
    pub sensitive: bool,
    pub writable: bool,
    /// The owning feature's check of a complete value.
    pub validate: fn(&str) -> bool,
}

/// The namespaces the running binary knows. Anything else is unknown.
#[derive(Debug, Clone, Default)]
pub struct SettingsCatalog {
    namespaces: Vec<SettingsNamespace>,
}

impl SettingsCatalog {
    pub fn new(namespaces: impl IntoIterator<Item = SettingsNamespace>) -> Self {
        Self {
            namespaces: namespaces.into_iter().collect(),
        }
    }

    pub fn namespace(&self, name: &str) -> Option<&SettingsNamespace> {
        self.namespaces
            .iter()
            .find(|namespace| namespace.name == name)
    }

    fn readable(&self, name: &str) -> bool {
        self.namespace(name)
            .is_some_and(|namespace| !namespace.sensitive)
    }
}

impl OrgStore {
    /// Every stored, readable namespace and its value.
    pub fn list_settings(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        catalog: &SettingsCatalog,
    ) -> Result<BTreeMap<String, String>, OrgError> {
        let request = OperationRequest::new("settings.list").in_workspace(workspace.clone());
        self.read(caller, request, |authorized| {
            authorized.settings(workspace, catalog)
        })
    }

    /// One namespace's value, `None` when unset.
    pub fn get_setting(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        catalog: &SettingsCatalog,
        namespace: &str,
    ) -> Result<Option<String>, OrgError> {
        let request = OperationRequest::new("settings.get").in_workspace(workspace.clone());
        self.read(caller, request, |authorized| {
            authorized.setting(workspace, catalog, namespace)
        })
    }

    /// Replaces a writable namespace's whole value. Repeating the stored value
    /// changes nothing and records no entry. Values never enter the audit log.
    pub fn replace_setting(
        &self,
        caller: &Caller,
        workspace: &WorkspaceId,
        catalog: &SettingsCatalog,
        namespace: &str,
        value: &str,
    ) -> Result<(), OrgError> {
        let request = OperationRequest::new("settings.replace").in_workspace(workspace.clone());
        self.write(caller, request, |authorized| {
            let entry = catalog
                .namespace(namespace)
                .filter(|entry| entry.writable)
                .ok_or(OrgError::invalid("namespace"))?;
            if !(entry.validate)(value) {
                return Err(OrgError::invalid("value"));
            }
            if authorized.stored(workspace, namespace)?.as_deref() == Some(value) {
                return Ok(());
            }
            authorized
                .transaction
                .execute(
                    "INSERT INTO org_settings (workspace, namespace, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT (workspace, namespace) DO UPDATE SET value = excluded.value",
                    params![workspace.as_str(), namespace, value],
                )
                .storage()?;
            authorized.record(
                workspace,
                Change {
                    action: "settings.replace",
                    target_kind: "settings",
                    target: namespace,
                    label: None,
                    details: String::new(),
                },
            )
        })
    }
}

impl Authorized<'_> {
    pub(super) fn settings(
        &self,
        workspace: &WorkspaceId,
        catalog: &SettingsCatalog,
    ) -> Result<BTreeMap<String, String>, OrgError> {
        let mut statement = self
            .transaction
            .prepare("SELECT namespace, value FROM org_settings WHERE workspace = ?1")
            .storage()?;
        let rows = statement
            .query_map([workspace.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .storage()?;
        let mut settings = BTreeMap::new();
        for row in rows {
            let (namespace, value) = row.storage()?;
            if catalog.readable(&namespace) {
                settings.insert(namespace, value);
            }
        }
        Ok(settings)
    }

    /// Unknown and sensitive namespaces are both `not_found`.
    pub(super) fn setting(
        &self,
        workspace: &WorkspaceId,
        catalog: &SettingsCatalog,
        namespace: &str,
    ) -> Result<Option<String>, OrgError> {
        if !catalog.readable(namespace) {
            return Err(OrgError::NotFound {
                what: "namespace",
                cause: None,
            });
        }
        self.stored(workspace, namespace)
    }

    fn stored(&self, workspace: &WorkspaceId, namespace: &str) -> Result<Option<String>, OrgError> {
        self.transaction
            .query_row(
                "SELECT value FROM org_settings WHERE workspace = ?1 AND namespace = ?2",
                params![workspace.as_str(), namespace],
                |row| row.get(0),
            )
            .optional()
            .storage()
    }
}
