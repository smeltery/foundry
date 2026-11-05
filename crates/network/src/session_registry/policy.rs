use super::{MAX_FOLDER_RULES, RegistryError, Result};
use std::path::{Component, Path, PathBuf};

/// Directory rules. The deepest matching rule wins; default is allow.
/// Re-evaluated on every visibility, association and delivery operation.
///
/// A session is judged by its path as given (made absolute and with `.` and
/// `..` folded lexically, links not followed), by its resolved path, and by each
/// spelling that resolves one ancestor of the given path and keeps the rest. It
/// is excluded if any judgement excludes it, so a link inside an excluded folder
/// cannot carry a session out, even when the rule and the session reach that
/// folder through differently linked ancestors, and a link into one cannot
/// carry it in.
/// A rule matches a path under either its written or its resolved form. Paths
/// that do not exist are rejected as invalid before any rule is applied.
#[derive(Debug, Default)]
pub struct FolderPolicy {
    rules: Vec<Rule>,
}
#[derive(Debug)]
struct Rule {
    resolved: PathBuf,
    given: PathBuf,
    allow: bool,
}
impl FolderPolicy {
    pub fn new(rules: Vec<(PathBuf, bool)>) -> Result<Self> {
        if rules.len() > MAX_FOLDER_RULES {
            return Err(RegistryError::Limit);
        }
        let mut checked: Vec<Rule> = Vec::new();
        for (path, allow) in rules {
            let resolved = directory(&path)?;
            if checked.iter().any(|rule| rule.resolved == resolved) {
                return Err(RegistryError::InvalidPath);
            }
            let given = lexical(&path)?;
            checked.push(Rule {
                resolved,
                given,
                allow,
            });
        }
        Ok(Self { rules: checked })
    }
    pub(super) fn permits(&self, given: &Path, path: &Path) -> bool {
        let Ok(current) = directory(path) else {
            return false;
        };
        // A moved directory or changed symlink must not acquire the old identity.
        if current != path {
            return false;
        }
        if self.rules.is_empty() {
            return true;
        }
        // The root ancestor yields the given spelling; the full path yields `current`.
        given.ancestors().all(|ancestor| {
            let (Ok(resolved), Ok(rest)) = (ancestor.canonicalize(), given.strip_prefix(ancestor))
            else {
                return false;
            };
            self.allows(&resolved.join(rest))
        })
    }
    fn allows(&self, path: &Path) -> bool {
        self.rules
            .iter()
            .filter_map(|rule| {
                [&rule.resolved, &rule.given]
                    .into_iter()
                    .filter(|root| path.starts_with(root))
                    .map(|root| root.components().count())
                    .max()
                    .map(|depth| (depth, rule.allow))
            })
            .max_by_key(|(depth, allow)| (*depth, !*allow))
            .is_none_or(|(_, allow)| allow)
    }
}
pub(super) fn directory(path: &Path) -> Result<PathBuf> {
    super::types::text(path.to_str().ok_or(RegistryError::InvalidPath)?)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| RegistryError::InvalidPath)?;
    super::types::text(canonical.to_str().ok_or(RegistryError::InvalidPath)?)?;
    if !canonical.is_dir() {
        return Err(RegistryError::InvalidPath);
    }
    Ok(canonical)
}
/// The absolute path as written, with `.` and `..` folded without touching disk.
pub(super) fn lexical(path: &Path) -> Result<PathBuf> {
    super::types::text(path.to_str().ok_or(RegistryError::InvalidPath)?)?;
    let absolute = std::path::absolute(path).map_err(|_| RegistryError::InvalidPath)?;
    let mut folded = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    Ok(folded)
}
