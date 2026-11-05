//! Verifying what a mutation actually did. The helper's exit status is not
//! trusted on its own: after connect/disconnect/rename/delete/save the real
//! state is re-queried, and a timeout (unknown outcome) is reconciled by
//! looking at what actually happened.

use super::error::HelperError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Reported success and the new state is observed.
    Confirmed,
    /// Reported success, but the observed state is not the expected one.
    Mismatch,
    /// Reported failure (or timed out) and the change did not happen.
    Failed,
    /// Reported failure (or timed out) but the change is in effect.
    AppliedDespiteError,
    /// The real state could not be determined.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    pub verdict: Verdict,
    pub message: String,
    /// The helper's own result was unknown (timeout / interrupted pipe).
    pub outcome_was_unknown: bool,
}

impl Verified {
    /// The intended change is in effect.
    pub fn applied(&self) -> bool {
        matches!(
            self.verdict,
            Verdict::Confirmed | Verdict::AppliedDespiteError
        )
    }

    pub fn into_result(self) -> Result<String, String> {
        if self.applied() {
            Ok(self.message)
        } else {
            Err(self.message)
        }
    }
}

/// Wording for one kind of change, e.g. activating `wg0`.
pub struct ChangeText {
    /// Success message: "Activated wg0".
    pub done: String,
    /// Noun for the attempt: "Activation".
    pub attempt: &'static str,
    /// The expected state, as a clause: "wg0 is up".
    pub expected: String,
    /// The opposite clause: "wg0 is not up".
    pub unexpected: String,
}

/// Combine the helper's result with an observation of the real state.
/// `observe` returns whether the expected state holds.
pub fn verify_change(
    text: &ChangeText,
    action: Result<(), HelperError>,
    explain: impl Fn(&HelperError) -> String,
    observe: impl FnOnce() -> Result<bool, HelperError>,
) -> Verified {
    let outcome_was_unknown = action.as_ref().is_err_and(HelperError::outcome_unknown);
    let observed = observe();
    let (verdict, message) = match (action, observed) {
        (Ok(()), Ok(true)) => (Verdict::Confirmed, text.done.clone()),
        (Ok(()), Ok(false)) => (
            Verdict::Mismatch,
            format!(
                "{} reported success, but {} — check the Log tab.",
                text.attempt, text.unexpected
            ),
        ),
        (Ok(()), Err(e)) => (
            Verdict::Unknown,
            format!("{}, but the result couldn't be re-checked: {e}", text.done),
        ),
        (Err(e), Ok(true)) if e.outcome_unknown() => (
            Verdict::AppliedDespiteError,
            format!("{} timed out, but {} now.", text.attempt, text.expected),
        ),
        (Err(e), Ok(true)) => (
            Verdict::AppliedDespiteError,
            format!(
                "{} reported an error, but {}: {}",
                text.attempt,
                text.expected,
                explain(&e)
            ),
        ),
        (Err(e), Ok(false)) if e.outcome_unknown() => (
            Verdict::Failed,
            format!("{} timed out; {}.", text.attempt, text.unexpected),
        ),
        (Err(e), Ok(false)) => (
            Verdict::Failed,
            format!("{} failed: {}", text.attempt, explain(&e)),
        ),
        (Err(e), Err(check)) if e.outcome_unknown() => (
            Verdict::Unknown,
            format!(
                "{} timed out and the result couldn't be re-checked ({check}); refresh to see the real state.",
                text.attempt
            ),
        ),
        (Err(e), Err(_)) => (
            Verdict::Failed,
            format!("{} failed: {}", text.attempt, explain(&e)),
        ),
    };
    Verified {
        verdict,
        message,
        outcome_was_unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> ChangeText {
        ChangeText {
            done: "Activated wg0".into(),
            attempt: "Activation",
            expected: "wg0 is up".into(),
            unexpected: "wg0 is not up".into(),
        }
    }

    fn failed() -> HelperError {
        HelperError::from_failure("wg-helper up wg0".into(), "RTNETLINK answers: busy".into())
    }

    fn check(action: Result<(), HelperError>, observed: Result<bool, HelperError>) -> Verified {
        verify_change(&text(), action, |e| e.to_string(), || observed)
    }

    #[test]
    fn success_is_only_claimed_when_observed() {
        let v = check(Ok(()), Ok(true));
        assert_eq!(v.verdict, Verdict::Confirmed);
        assert_eq!(v.message, "Activated wg0");
        let v = check(Ok(()), Ok(false));
        assert_eq!(v.verdict, Verdict::Mismatch);
        assert!(!v.applied());
        assert!(v.message.contains("wg0 is not up"));
        let v = check(Ok(()), Err(HelperError::TimedOut));
        assert_eq!(v.verdict, Verdict::Unknown);
        assert!(v.clone().into_result().is_err());
    }

    #[test]
    fn timeouts_are_reconciled_from_the_real_state() {
        let v = check(Err(HelperError::TimedOut), Ok(true));
        assert_eq!(v.verdict, Verdict::AppliedDespiteError);
        assert!(v.outcome_was_unknown && v.applied());
        assert_eq!(v.message, "Activation timed out, but wg0 is up now.");
        let v = check(Err(HelperError::TimedOut), Ok(false));
        assert_eq!(v.verdict, Verdict::Failed);
        assert_eq!(v.message, "Activation timed out; wg0 is not up.");
        let v = check(Err(HelperError::TimedOut), Err(HelperError::TimedOut));
        assert_eq!(v.verdict, Verdict::Unknown);
        assert!(v.message.contains("refresh"));
    }

    #[test]
    fn errors_with_an_applied_change_are_reported_as_such() {
        let v = check(Err(failed()), Ok(true));
        assert_eq!(v.verdict, Verdict::AppliedDespiteError);
        assert!(v.message.contains("reported an error, but wg0 is up"));
        let v = check(Err(failed()), Ok(false));
        assert_eq!(v.verdict, Verdict::Failed);
        assert!(v.message.starts_with("Activation failed: "));
        assert!(!v.outcome_was_unknown);
    }
}
