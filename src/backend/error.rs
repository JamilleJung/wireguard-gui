//! Typed errors for privileged helper invocation. `Display` keeps the wording
//! users saw before the errors were typed.

use std::fmt;

/// stderr fragment the helper prints when another mutation holds its lock.
const BUSY_MARKER: &str = "another helper operation is busy";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperError {
    /// No trusted helper executable could be located.
    NotInstalled,
    /// Background reads under pkexec need a pre-existing authorization.
    AuthorizationRequired,
    /// The helper (or sudo/pkexec) could not be started.
    Spawn(String),
    /// The helper did not finish in time; whatever it was doing may or may
    /// not have happened.
    TimedOut,
    /// Another helper mutation held the shared lock.
    Busy { command: String, stderr: String },
    /// The helper ran and reported failure.
    Failed { command: String, stderr: String },
    /// The helper wrote more than the capture limit and was stopped.
    OutputTooLarge,
    /// The helper's stdout was not UTF-8.
    InvalidUtf8,
    /// The helper exited successfully without reading the whole payload.
    IncompleteInput,
    /// Local pipe/process plumbing failed while the helper was running.
    Io(String),
    /// The helper answered, but the answer was malformed.
    InvalidOutput(String),
}

impl HelperError {
    pub(crate) fn from_failure(command: String, stderr: String) -> Self {
        if stderr.contains(BUSY_MARKER) {
            Self::Busy { command, stderr }
        } else {
            Self::Failed { command, stderr }
        }
    }

    /// True when the helper may have completed (part of) the action even
    /// though no success was reported: the real state must be re-queried.
    pub fn outcome_unknown(&self) -> bool {
        matches!(
            self,
            Self::TimedOut | Self::IncompleteInput | Self::OutputTooLarge | Self::Io(_)
        )
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::TimedOut)
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }

    /// The helper's own stderr text, when it ran and failed.
    pub fn stderr(&self) -> Option<&str> {
        match self {
            Self::Busy { stderr, .. } | Self::Failed { stderr, .. } => Some(stderr),
            _ => None,
        }
    }

    /// Short machine-friendly name for structured logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotInstalled => "not-installed",
            Self::AuthorizationRequired => "authorization-required",
            Self::Spawn(_) => "spawn",
            Self::TimedOut => "timeout",
            Self::Busy { .. } => "busy",
            Self::Failed { .. } => "failed",
            Self::OutputTooLarge => "output-too-large",
            Self::InvalidUtf8 => "invalid-utf8",
            Self::IncompleteInput => "incomplete-input",
            Self::Io(_) => "io",
            Self::InvalidOutput(_) => "invalid-output",
        }
    }
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => {
                f.write_str("No trusted executable wg-helper found; run ./install.sh to install it.")
            }
            Self::AuthorizationRequired => f.write_str(
                "Helper authorization is required; run ./install.sh to configure the scoped helper grant.",
            ),
            Self::Spawn(message) | Self::Io(message) | Self::InvalidOutput(message) => {
                f.write_str(message)
            }
            Self::TimedOut => f.write_str("Helper operation timed out."),
            Self::Busy { command, stderr } | Self::Failed { command, stderr } => {
                write!(f, "{command}: {}", stderr.trim())
            }
            Self::OutputTooLarge => f.write_str("Helper output exceeds the 4 MiB safety limit."),
            Self::InvalidUtf8 => f.write_str("wg-helper returned invalid UTF-8."),
            Self::IncompleteInput => {
                f.write_str("Could not send the complete config to wg-helper.")
            }
        }
    }
}

impl std::error::Error for HelperError {}

impl From<HelperError> for String {
    fn from(error: HelperError) -> Self {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_keeps_user_facing_wording() {
        assert_eq!(
            HelperError::TimedOut.to_string(),
            "Helper operation timed out."
        );
        assert!(
            HelperError::NotInstalled
                .to_string()
                .contains("./install.sh")
        );
        let failed = HelperError::from_failure("wg-helper up wg0".into(), "boom\n".into());
        assert_eq!(failed.to_string(), "wg-helper up wg0: boom");
        assert!(!failed.outcome_unknown());
    }

    #[test]
    fn busy_lock_and_unknown_outcomes_are_classified() {
        let busy = HelperError::from_failure(
            "wg-helper save wg0".into(),
            "wg-helper: another helper operation is busy; retry when it completes".into(),
        );
        assert!(busy.is_busy());
        assert!(!busy.outcome_unknown());
        assert!(HelperError::TimedOut.outcome_unknown());
        assert!(HelperError::IncompleteInput.outcome_unknown());
        assert!(!HelperError::AuthorizationRequired.outcome_unknown());
    }
}
