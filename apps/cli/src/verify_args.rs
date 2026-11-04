//! Completion criteria flags shared by the task commands (completion-policy.md).
//!
//! Flags build the inline `completion` criteria of the session contract; the
//! contract validates bounds and workspace confinement. Commands are argv
//! lists that never pass through a shell.
use crate::CommandError;
use clap::{Arg, ArgAction, ArgMatches, Args, Command, FromArgMatches};
use foundry_contracts::{CompletionCheck, CompletionCriteria};
use serde::{Serialize, Serializer, ser::SerializeMap};

/// Used when `--verify-timeout` is absent.
const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

/// Parsed by hand: each `--verify-command` occurrence keeps its own argv.
#[derive(Debug, Default, Clone)]
pub struct VerifyOptions {
    pub commands: Vec<Vec<String>>,
    pub timeout: Option<u64>,
    pub files: Vec<String>,
    pub digests: Vec<String>,
}

const COMMAND: &str = "verify_command";
const TIMEOUT: &str = "verify_timeout";
const FILE: &str = "verify_file";
const DIGEST: &str = "verify_sha256";

impl Args for VerifyOptions {
    fn augment_args(command: Command) -> Command {
        command
            .arg(
                Arg::new(COMMAND)
                    .long("verify-command")
                    .value_name("ARGV")
                    .num_args(1..)
                    .allow_hyphen_values(true)
                    .value_terminator(";")
                    .action(ArgAction::Append)
                    .help("Command whose exit status 0 verifies the task; end its argv with ';' when options follow. Repeatable"),
            )
            .arg(
                Arg::new(TIMEOUT)
                    .long("verify-timeout")
                    .value_name("SECS")
                    .value_parser(clap::value_parser!(u64).range(1..=600))
                    .help("Seconds each verify command may run, 1 to 600 (default 300)"),
            )
            .arg(
                Arg::new(FILE)
                    .long("verify-file")
                    .value_name("PATH")
                    .action(ArgAction::Append)
                    .help("Workspace-relative file that must exist. Repeatable"),
            )
            .arg(
                Arg::new(DIGEST)
                    .long("verify-sha256")
                    .value_name("HEX")
                    .action(ArgAction::Append)
                    .help("SHA-256 each --verify-file must match, paired in order"),
            )
    }
    fn augment_args_for_update(command: Command) -> Command {
        Self::augment_args(command)
    }
}

impl FromArgMatches for VerifyOptions {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let strings = |id| {
            matches
                .get_many::<String>(id)
                .map(|values| values.cloned().collect())
                .unwrap_or_default()
        };
        Ok(Self {
            commands: matches
                .get_occurrences::<String>(COMMAND)
                .map(|groups| groups.map(|argv| argv.cloned().collect()).collect())
                .unwrap_or_default(),
            timeout: matches.get_one::<u64>(TIMEOUT).copied(),
            files: strings(FILE),
            digests: strings(DIGEST),
        })
    }
    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl VerifyOptions {
    /// Inline criteria with stable IDs `command-N` and `file-N`, or `None` when
    /// no check was declared.
    pub fn criteria(&self) -> Result<Option<CompletionCriteria>, CommandError> {
        if self.commands.is_empty() && self.timeout.is_some() {
            return Err(usage("--verify-timeout requires --verify-command"));
        }
        if !self.digests.is_empty() && self.digests.len() != self.files.len() {
            return Err(usage("give one --verify-sha256 per --verify-file, or none"));
        }
        let timeout_ms = self.timeout.unwrap_or(DEFAULT_TIMEOUT_SECONDS) * 1000;
        let commands =
            self.commands
                .iter()
                .enumerate()
                .map(|(index, argv)| CompletionCheck::Command {
                    id: format!("command-{}", index + 1),
                    argv: argv.clone(),
                    timeout_ms,
                });
        let files = self
            .files
            .iter()
            .enumerate()
            .map(|(index, path)| CompletionCheck::File {
                id: format!("file-{}", index + 1),
                path: path.clone(),
                sha256: self.digests.get(index).cloned(),
            });
        let checks: Vec<_> = commands.chain(files).collect();
        if checks.is_empty() {
            return Ok(None);
        }
        let criteria = CompletionCriteria {
            profile: None,
            checks,
        };
        criteria
            .validate()
            .map_err(|error| CommandError::new("invalid", error.to_string()))?;
        Ok(Some(criteria))
    }
}

/// Encodes as the request's `completion` field, omitted when no check is set.
impl Serialize for VerifyOptions {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let criteria = self
            .criteria()
            .map_err(|error| serde::ser::Error::custom(error.message))?;
        let mut map = serializer.serialize_map(None)?;
        if let Some(criteria) = criteria {
            map.serialize_entry("completion", &criteria)?;
        }
        map.end()
    }
}

fn usage(message: &str) -> CommandError {
    CommandError::new("usage", message)
}
