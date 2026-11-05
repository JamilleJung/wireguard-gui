//! Operation state machine for background actions.
//!
//! Idle → Running → Succeeded | Failed | Unknown(timeout) → Reconciled → Idle
//!
//! Only one operation runs at a time: a new one can start only from Idle, so
//! mutations never overlap. An Unknown outcome must be reconciled (the real
//! state re-queried) before the operation is released.

use std::sync::Mutex;

use crate::logging;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Running,
    Succeeded,
    Failed,
    /// The action's result is unknown (e.g. the helper timed out).
    Unknown,
    /// The real state was re-checked after the action finished.
    Reconciled,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Reconciled => "reconciled",
        }
    }
}

/// How an action finished, as far as the worker knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

#[derive(Debug)]
pub struct OpMachine {
    phase: Phase,
    label: String,
}

impl Default for OpMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl OpMachine {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Idle,
            label: String::new(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Start an operation; refused unless idle.
    pub fn begin(&mut self, label: &str) -> Result<(), Busy> {
        if self.phase != Phase::Idle {
            return Err(Busy);
        }
        self.label = label.to_string();
        self.set(Phase::Running);
        Ok(())
    }

    /// Record the worker's result. Only valid while running.
    pub fn complete(&mut self, outcome: Outcome) {
        if self.phase == Phase::Running {
            self.set(match outcome {
                Outcome::Succeeded => Phase::Succeeded,
                Outcome::Failed => Phase::Failed,
                Outcome::Unknown => Phase::Unknown,
            });
        }
    }

    /// The real state has been re-queried after an unknown (or any) result.
    pub fn reconcile(&mut self) {
        if matches!(
            self.phase,
            Phase::Unknown | Phase::Succeeded | Phase::Failed
        ) {
            self.set(Phase::Reconciled);
        }
    }

    /// The completion was applied to the UI; the next operation may start.
    /// A still-running or unreconciled-unknown operation is recorded as failed
    /// / reconciled first so the history never skips a state.
    pub fn release(&mut self) {
        if self.phase == Phase::Running {
            self.set(Phase::Failed);
        }
        if self.phase == Phase::Unknown {
            self.set(Phase::Reconciled);
        }
        self.set(Phase::Idle);
        self.label.clear();
    }

    fn set(&mut self, phase: Phase) {
        if self.phase != phase {
            logging::log(
                "operation",
                &[
                    ("label", &self.label),
                    ("from", self.phase.as_str()),
                    ("to", phase.as_str()),
                ],
            );
            self.phase = phase;
        }
    }
}

static OPS: Mutex<OpMachine> = Mutex::new(OpMachine::new());

fn with<T>(f: impl FnOnce(&mut OpMachine) -> T) -> T {
    let mut guard = OPS.lock().unwrap_or_else(|poison| poison.into_inner());
    f(&mut guard)
}

pub fn current_phase() -> Phase {
    with(|m| m.phase())
}

/// Exclusive right to run one operation; releases the machine on drop (so a
/// panicking worker cannot wedge every later action).
pub struct OpGuard(());

impl OpGuard {
    pub fn acquire(label: &str) -> Option<Self> {
        with(|m| m.begin(label)).ok().map(|()| Self(()))
    }

    pub fn complete(&self, outcome: Outcome) {
        with(|m| m.complete(outcome));
    }

    pub fn reconcile(&self) {
        with(|m| m.reconcile());
    }
}

impl Drop for OpGuard {
    fn drop(&mut self) {
        with(|m| m.release());
    }
}

/// Classifies a worker's return value for the state machine.
pub trait TaskOutcome {
    fn outcome(&self) -> Outcome;
    /// Whether the worker re-queried the real state before returning.
    fn reconciled(&self) -> bool {
        false
    }
}

impl<T> TaskOutcome for Result<T, String> {
    fn outcome(&self) -> Outcome {
        if self.is_ok() {
            Outcome::Succeeded
        } else {
            Outcome::Failed
        }
    }
}

impl<T> TaskOutcome for Result<T, crate::backend::HelperError> {
    fn outcome(&self) -> Outcome {
        match self {
            Ok(_) => Outcome::Succeeded,
            Err(e) if e.outcome_unknown() => Outcome::Unknown,
            Err(_) => Outcome::Failed,
        }
    }
}

impl TaskOutcome for crate::backend::Verified {
    fn outcome(&self) -> Outcome {
        if self.outcome_was_unknown {
            Outcome::Unknown
        } else if self.applied() {
            Outcome::Succeeded
        } else {
            Outcome::Failed
        }
    }
    fn reconciled(&self) -> bool {
        true
    }
}

#[cfg(test)]
pub(crate) static GUARD_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_follow_the_documented_path() {
        let mut m = OpMachine::new();
        assert_eq!(m.phase(), Phase::Idle);
        m.begin("Saving").unwrap();
        assert_eq!(m.phase(), Phase::Running);
        assert_eq!(m.begin("Other"), Err(Busy));
        m.complete(Outcome::Unknown);
        assert_eq!(m.phase(), Phase::Unknown);
        // Still exclusive until reconciled and released.
        assert_eq!(m.begin("Other"), Err(Busy));
        m.reconcile();
        assert_eq!(m.phase(), Phase::Reconciled);
        assert_eq!(m.begin("Other"), Err(Busy));
        m.release();
        assert_eq!(m.phase(), Phase::Idle);
        m.begin("Other").unwrap();
        assert_eq!(m.label, "Other");
    }

    #[test]
    fn invalid_transitions_are_ignored() {
        let mut m = OpMachine::new();
        m.complete(Outcome::Succeeded);
        assert_eq!(m.phase(), Phase::Idle);
        m.reconcile();
        assert_eq!(m.phase(), Phase::Idle);
        m.begin("x").unwrap();
        m.reconcile();
        assert_eq!(m.phase(), Phase::Running);
        m.complete(Outcome::Failed);
        m.complete(Outcome::Succeeded);
        assert_eq!(m.phase(), Phase::Failed);
    }

    #[test]
    fn task_results_map_to_outcomes() {
        use crate::backend::{HelperError, Verdict, Verified};
        assert_eq!(Ok::<(), String>(()).outcome(), Outcome::Succeeded);
        assert_eq!(Err::<(), String>("x".into()).outcome(), Outcome::Failed);
        assert_eq!(
            Err::<(), _>(HelperError::TimedOut).outcome(),
            Outcome::Unknown
        );
        let v = Verified {
            verdict: Verdict::AppliedDespiteError,
            message: String::new(),
            outcome_was_unknown: true,
        };
        assert_eq!(v.outcome(), Outcome::Unknown);
        assert!(v.reconciled());
    }

    #[test]
    fn mutation_guard_remains_busy_while_completion_is_queued() {
        let _lock = GUARD_TEST_LOCK.lock().unwrap();
        let guard = OpGuard::acquire("test").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(guard).unwrap())
            .join()
            .unwrap();
        assert!(OpGuard::acquire("test").is_none());
        drop(rx.recv().unwrap());
        assert!(OpGuard::acquire("test").is_some());
    }

    #[test]
    fn mutation_guard_excludes_overlapping_workers_and_releases_on_drop() {
        let _lock = GUARD_TEST_LOCK.lock().unwrap();
        let guard = OpGuard::acquire("test").unwrap();
        assert!(
            std::thread::spawn(|| OpGuard::acquire("other").is_none())
                .join()
                .unwrap()
        );
        guard.complete(Outcome::Unknown);
        assert_eq!(current_phase(), Phase::Unknown);
        drop(guard);
        assert_eq!(current_phase(), Phase::Idle);
        assert!(OpGuard::acquire("test").is_some());
    }
}
