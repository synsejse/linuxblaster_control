//! Pending controls and a projected view; only Apply sends a write to the worker.
use super::{Command, EqContext, X4App};
use soundblaster_x4::{Feature, Output, State};

impl Command {
    fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Output(_), Self::Output(_))
            | (Self::Sxfi(..), Self::Sxfi(..))
            | (Self::Profile(..), Self::Profile(..))
            | (Self::MicrophoneBoost(_), Self::MicrophoneBoost(_)) => true,
            (Self::Feature(a, _), Self::Feature(b, _)) => a == b,
            (Self::MixerMute(a, _), Self::MixerMute(b, _)) => a == b,
            _ => false,
        }
    }
    pub(super) fn changes_context(&self) -> bool {
        matches!(
            self,
            Self::Output(_)
                | Self::Sxfi(..)
                | Self::Profile(..)
                | Self::Feature(Feature::DirectMode, _)
        )
    }
    fn changed(&self, state: &State) -> bool {
        match self {
            Self::Output(value) => state.output != *value,
            Self::Feature(feature, value) => state.feature(*feature) != *value,
            Self::Sxfi(_, value) => state.sxfi != Some(*value),
            Self::Profile(_, value) => state.active_eq_profile != Some(*value),
            Self::MixerMute(index, value) => state
                .mixer
                .iter()
                .find(|control| control.index == *index)
                .is_some_and(|control| control.muted != Some(*value)),
            Self::MicrophoneBoost(value) => state
                .mixer
                .iter()
                .find(|control| control.is_microphone_boost())
                .is_some_and(|control| {
                    control.microphone_boost != Some(*value)
                }),
            _ => false,
        }
    }
    fn rank(&self) -> u8 {
        match self {
            Self::Output(_) => 0,
            Self::Feature(Feature::DirectMode, false) => 1,
            Self::Sxfi(..) => 2,
            Self::Profile(..) => 3,
            Self::Feature(..) => 4,
            Self::Eq(..) => 5,
            _ => 6,
        }
    }
    pub(super) fn rebased(&self, state: &State) -> Self {
        let context = EqContext::from_state(state);
        match self {
            Self::Sxfi(_, mode) => Self::Sxfi(context, *mode),
            Self::Profile(_, index) => Self::Profile(context, *index),
            _ => self.clone(),
        }
    }
}

pub(super) fn project(state: &mut State, change: &Command) {
    match change {
        Command::Output(value) => state.output = *value,
        Command::Feature(feature, value) => {
            let mask = 1 << feature.bit();
            if *value {
                state.features |= mask;
            } else {
                state.features &= !mask;
            }
        }
        Command::Sxfi(_, mode) => {
            state.sxfi = Some(*mode);
            // Firmware chooses a slot when changing modes. Do not present the
            // old curve as the new slot's EQ before its readback.
            state.active_eq_profile = None;
        }
        Command::Profile(_, index) => state.active_eq_profile = Some(*index),
        Command::MixerMute(index, value) => {
            if let Some(control) = state
                .mixer
                .iter_mut()
                .find(|control| control.index == *index)
            {
                control.muted = Some(*value);
            }
        }
        Command::MicrophoneBoost(value) => {
            if let Some(control) = state
                .mixer
                .iter_mut()
                .find(|control| control.is_microphone_boost())
            {
                control.microphone_boost = Some(*value);
            }
        }
        _ => {}
    }
}

/// Check the entire plan before the worker performs its first write.
pub(super) fn validate_changes(
    state: &State,
    changes: &[Command],
) -> Result<(), String> {
    let mut projected = state.clone();
    for change in changes {
        let valid = match change {
            Command::Output(_) => true,
            Command::Feature(feature, _) => {
                projected.writable_feature(*feature)
                    && (*feature != Feature::HeadphoneHighGain
                        || projected.output == Output::Headphone)
            }
            Command::Sxfi(_, _) => {
                projected.sxfi.is_some()
                    && projected.output == Output::Headphone
                    && !projected.feature(Feature::DirectMode)
            }
            Command::Profile(_, index) => {
                projected.eq_profile_available(*index)
                    && projected
                        .eq_profiles
                        .iter()
                        .any(|profile| profile.index == *index)
            }
            Command::Eq(context, eq) => {
                !projected.feature(Feature::DirectMode)
                    && projected.eq_supported
                    && context.check(&projected).is_ok()
                    && eq.validate().is_ok()
            }
            Command::MixerLevel(index, value) => {
                value.is_finite()
                    && projected.mixer.iter().any(|control| {
                        control.index == *index
                            && control.range.as_ref().is_some_and(|range| {
                                *value >= range.min_db && *value <= range.max_db
                            })
                    })
            }
            Command::MixerMute(index, _) => {
                projected.mixer.iter().any(|control| {
                    control.index == *index && control.mute_available()
                })
            }
            Command::MicrophoneBoost(_) => {
                projected.mixer.iter().any(|control| {
                    control.is_microphone_boost()
                        && control.microphone_boost.is_some()
                })
            }
            Command::Refresh | Command::Apply(..) => false,
        };
        if !valid {
            return Err("A pending setting is unavailable. Revert or refresh before applying.".into());
        }
        project(&mut projected, change);
    }
    Ok(())
}

impl X4App {
    pub(super) fn context_pending(&self) -> bool {
        self.control_drafts.iter().any(Command::changes_context)
    }
    pub(super) fn rebuild_preview(&mut self) {
        self.preview.clone_from(&self.state);
        if let Some(preview) = &mut self.preview {
            let mut changes = self.control_drafts.clone();
            changes.sort_by_key(Command::rank);
            for change in &changes {
                project(preview, change);
            }
        }
    }
    pub(super) fn stage(&mut self, change: Command) {
        if self.busy() || self.offline {
            return;
        }
        if matches!(change, Command::Refresh) {
            self.send(change);
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        if matches!(
            change,
            Command::Eq(..) | Command::Apply(..) | Command::MixerLevel(..)
        ) || !change.changed(self.preview.as_ref().unwrap_or(state))
        {
            return;
        }
        if change.changes_context() {
            if self.dirty {
                self.draft.clone_from(&state.eq);
                self.dirty = false;
                self.notice = Some("EQ edits were discarded. Apply the output, mode or profile selection before editing its EQ.".into());
            }
            match change {
                Command::Output(_) => self.control_drafts.retain(|old| {
                    !matches!(
                        old,
                        Command::Sxfi(..)
                            | Command::Profile(..)
                            | Command::Feature(Feature::HeadphoneHighGain, _)
                    )
                }),
                Command::Sxfi(..)
                | Command::Feature(Feature::DirectMode, true) => {
                    self.control_drafts.retain(|old| {
                        !matches!(old, Command::Profile(..) | Command::Sxfi(..))
                    });
                }
                _ => {}
            }
        }
        self.control_drafts.retain(|old| !old.same_target(&change));
        if change.changed(state) {
            self.control_drafts.push(change);
        }
        self.rebuild_preview();
    }
    pub(super) fn planned_changes(&self) -> Result<Vec<Command>, String> {
        let state = self.state.as_ref().ok_or("Device is unavailable")?;
        let mut changes = self.control_drafts.clone();
        for control in &state.mixer {
            if let Some(value) = self.mixer_drafts.get(&control.index)
                && control
                    .level_db
                    .is_some_and(|actual| (actual - value).abs() > 0.001)
            {
                changes.push(Command::MixerLevel(control.index, *value));
            }
        }
        if self.dirty && !self.context_pending() {
            changes.push(Command::Eq(
                self.draft_context.ok_or("EQ is unavailable")?,
                self.draft.clone().ok_or("EQ is unavailable")?,
            ));
        }
        changes.sort_by_key(Command::rank);
        validate_changes(state, &changes)?;
        Ok(changes)
    }
    pub(super) fn has_changes(&self) -> bool {
        self.dirty
            || !self.control_drafts.is_empty()
            || self.state.as_ref().is_some_and(|state| {
                state.mixer.iter().any(|control| {
                    self.mixer_drafts
                        .get(&control.index)
                        .zip(control.level_db)
                        .is_some_and(|(value, actual)| {
                            (*value - actual).abs() > 0.001
                        })
                })
            })
    }
    pub(super) fn apply(&mut self) {
        if self.busy() || self.offline {
            return;
        }
        match self.planned_changes() {
            Ok(changes) if !changes.is_empty() => {
                let context = EqContext::from_state(
                    self.state.as_ref().expect("validated device"),
                );
                self.send(Command::Apply(context, changes));
            }
            Ok(_) => {}
            Err(error) => self.error = Some(error),
        }
    }
    pub(super) fn revert(&mut self) {
        if self.busy() {
            return;
        }
        self.control_drafts.clear();
        self.mixer_drafts.clear();
        self.mixer_editing.clear();
        self.dirty = false;
        if let Some(state) = &self.state {
            self.draft.clone_from(&state.eq);
            self.draft_context = Some(EqContext::from_state(state));
        }
        self.rebuild_preview();
    }
}
