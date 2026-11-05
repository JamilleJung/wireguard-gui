//! The editor draft's lifecycle. A draft is never discarded implicitly: when
//! validation or the helper save fails, the text stays in the editor and the
//! user chooses Retry, Re-edit or Discard. Closing the window while a failed
//! or in-flight save exists keeps the draft and asks instead.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The config or name did not pass local validation.
    Validation,
    /// The helper refused or could not save (permission, busy lock, I/O, …).
    Save,
    /// The save timed out and the file did not match afterwards.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftState {
    Editing,
    Saving,
    Failed { kind: FailureKind, message: String },
    Saved,
    Discarded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    Retry,
    ReEdit,
    Discard,
}

/// What the UI should do after a recovery choice or a close request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Submit the current text again.
    Resubmit,
    /// Hide the recovery bar and return focus to the editor (text unchanged).
    Focus,
    /// Close the window; the draft is gone.
    Close,
    /// Keep the window open and show the recovery choices.
    Ask,
}

#[derive(Debug)]
pub struct Draft {
    state: DraftState,
}

impl Default for Draft {
    fn default() -> Self {
        Self::new()
    }
}

impl Draft {
    pub fn new() -> Self {
        Self {
            state: DraftState::Editing,
        }
    }

    pub fn state(&self) -> &DraftState {
        &self.state
    }

    /// Begin a save. Refused while one is already in flight.
    pub fn submit(&mut self) -> bool {
        if matches!(self.state, DraftState::Saving | DraftState::Discarded) {
            return false;
        }
        self.state = DraftState::Saving;
        true
    }

    pub fn fail(&mut self, kind: FailureKind, message: impl Into<String>) {
        if self.state != DraftState::Discarded {
            self.state = DraftState::Failed {
                kind,
                message: message.into(),
            };
        }
    }

    pub fn saved(&mut self) {
        self.state = DraftState::Saved;
    }

    /// A later edit after an earlier draft was saved: back to editing.
    pub fn edited(&mut self) {
        if self.state == DraftState::Saved {
            self.state = DraftState::Editing;
        }
    }

    pub fn has_failure(&self) -> bool {
        matches!(self.state, DraftState::Failed { .. })
    }

    /// The recovery choices to offer right now (the editor's recovery bar).
    #[cfg(test)]
    pub fn options(&self) -> &'static [Recovery] {
        match self.state {
            DraftState::Failed { .. } => &[Recovery::Retry, Recovery::ReEdit, Recovery::Discard],
            _ => &[],
        }
    }

    pub fn choose(&mut self, choice: Recovery) -> Effect {
        match choice {
            // The caller resubmits (which moves the draft to Saving).
            Recovery::Retry if self.has_failure() => Effect::Resubmit,
            Recovery::Retry => Effect::Ask,
            Recovery::ReEdit => {
                if self.has_failure() {
                    self.state = DraftState::Editing;
                }
                Effect::Focus
            }
            Recovery::Discard => {
                self.state = DraftState::Discarded;
                Effect::Close
            }
        }
    }

    /// The user pressed Cancel or closed the window.
    pub fn close_requested(&mut self) -> Effect {
        match self.state {
            DraftState::Saving => Effect::Ask,
            DraftState::Failed { .. } => Effect::Ask,
            _ => {
                self.state = DraftState::Discarded;
                Effect::Close
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_failure_keeps_the_draft_and_offers_recovery() {
        let mut d = Draft::new();
        assert!(d.submit());
        d.fail(FailureKind::Validation, "Missing PrivateKey");
        assert_eq!(
            d.options(),
            &[Recovery::Retry, Recovery::ReEdit, Recovery::Discard]
        );
        assert_eq!(d.close_requested(), Effect::Ask);
        assert!(d.has_failure());
        assert_eq!(d.choose(Recovery::ReEdit), Effect::Focus);
        assert_eq!(d.state(), &DraftState::Editing);
        assert!(d.options().is_empty());
    }

    #[test]
    fn helper_failure_never_loses_the_draft_until_discarded() {
        let mut d = Draft::new();
        d.submit();
        d.fail(FailureKind::Save, "another helper operation is busy");
        assert_eq!(d.close_requested(), Effect::Ask);
        assert_eq!(d.choose(Recovery::Retry), Effect::Resubmit);
        assert!(d.submit());
        assert_eq!(d.state(), &DraftState::Saving);
        assert_eq!(d.choose(Recovery::Retry), Effect::Ask);
        // No second submit while one is in flight, and closing is refused.
        assert!(!d.submit());
        assert_eq!(d.close_requested(), Effect::Ask);
        d.fail(FailureKind::Unknown, "timed out");
        assert_eq!(d.choose(Recovery::Discard), Effect::Close);
        assert_eq!(d.state(), &DraftState::Discarded);
        assert!(!d.submit());
    }

    #[test]
    fn unchanged_or_saved_drafts_close_normally() {
        let mut d = Draft::new();
        assert_eq!(d.close_requested(), Effect::Close);
        let mut d = Draft::new();
        d.submit();
        d.saved();
        d.edited();
        assert_eq!(d.state(), &DraftState::Editing);
        assert_eq!(d.close_requested(), Effect::Close);
    }
}
