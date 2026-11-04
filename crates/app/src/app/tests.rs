use super::*;
use soundblaster_x4::{
    EqProfileInfo, Feature, MixerControl, MixerGroup, MixerRange, Output,
    SxfiMode,
};
use std::time::Instant;

pub fn state() -> State {
    State {
        firmware: "test".into(),
        output: Output::Headphone,
        supported_features: 0x60,
        reset_features: 0,
        features: 0x40,
        eq_supported: true,
        eq: Some(Equalizer {
            enabled: true,
            preamp_db: -9.0,
            bands_db: [0.0; 10],
        }),
        sxfi: Some(SxfiMode::Off),
        eq_profiles: vec![EqProfileInfo {
            index: 0,
            name: "Music".into(),
        }],
        active_eq_profile: Some(0),
        mixer: vec![MixerControl {
            index: 1,
            name: "Headphones".into(),
            group: MixerGroup::Playback,
            descriptor: 0x89c2,
            has_volume: true,
            is_db: true,
            range: Some(MixerRange {
                min_db: -80.0,
                max_db: 0.0,
                step_db: 1.0,
                min_raw: -20480,
                max_raw: 0,
                step_raw: 256,
            }),
            level_db: Some(-6.0),
            raw_level: Some(-1536),
            muted: Some(false),
            microphone_boost: None,
        }],
    }
}
pub fn app() -> (X4App, Sender<Event>, Receiver<Command>) {
    let (tx, commands) = mpsc::channel();
    let (events, rx) = mpsc::channel();
    (X4App::with_channels(tx, rx), events, commands)
}
#[test]
fn mixer_refresh_preserves_eq_edits_but_context_change_discards_them() {
    let (mut app, events, _) = app();
    events
        .send(Event::State(state(), Completion::Refresh))
        .unwrap();
    app.events();
    app.draft.as_mut().unwrap().preamp_db = -6.0;
    app.dirty = true;
    events
        .send(Event::State(state(), Completion::Control))
        .unwrap();
    app.events();
    assert!(app.dirty);
    assert_eq!(app.draft.as_ref().unwrap().preamp_db, -6.0);
    let mut changed = state();
    changed.active_eq_profile = Some(1);
    events
        .send(Event::State(changed, Completion::Poll))
        .unwrap();
    app.events();
    assert!(!app.dirty);
    assert_eq!(app.draft.as_ref().unwrap().preamp_db, -9.0);
    assert!(app.notice.is_some());
}
#[test]
fn stale_context_rejected_for_mode_profile_output_and_direct_changes() {
    let original = state();
    let context = EqContext::from_state(&original);
    assert!(context.check(&original).is_ok());
    let mut changed = original.clone();
    changed.sxfi = Some(SxfiMode::Battle);
    assert!(context.check(&changed).is_err());
    changed = original.clone();
    changed.active_eq_profile = Some(1);
    assert!(context.check(&changed).is_err());
    changed = original.clone();
    changed.output = Output::Speaker;
    assert!(context.check(&changed).is_err());
    changed = original;
    changed.features |= 1 << Feature::DirectMode.bit();
    assert!(context.check(&changed).is_err());
}
#[test]
fn poll_preserves_an_active_mixer_edit_then_refreshes_after_release() {
    let (mut app, events, _) = app();
    events
        .send(Event::State(state(), Completion::Refresh))
        .unwrap();
    app.events();
    app.mixer_drafts.insert(1, -12.0);
    app.mixer_editing.insert(1);
    events
        .send(Event::State(state(), Completion::Poll))
        .unwrap();
    app.events();
    assert_eq!(app.mixer_drafts[&1], -12.0);
    app.mixer_editing.remove(&1);
    events
        .send(Event::State(state(), Completion::Poll))
        .unwrap();
    app.events();
    assert_eq!(app.mixer_drafts[&1], -6.0);
}
#[test]
fn profile_availability_follows_sxfi_and_direct_mode() {
    let mut current = state();
    assert!(current.eq_profile_available(0));
    assert!(current.eq_profile_available(2));
    assert!(!current.eq_profile_available(3));
    for mode in [SxfiMode::On, SxfiMode::Battle] {
        current.sxfi = Some(mode);
        assert!(current.eq_profile_available(3));
        assert!(!current.eq_profile_available(0));
    }
    current.features |= 1 << Feature::DirectMode.bit();
    assert!(!current.eq_profile_available(3));
    current.features &= !(1 << Feature::DirectMode.bit());
    current.sxfi = None;
    assert!(!current.eq_profile_available(0));
    assert!(!current.eq_profile_available(3));
}
#[test]
fn spdif_playback_mute_is_disabled_without_disabling_other_mutes() {
    let mut control = state().mixer.remove(0);
    assert!(control.mute_available());
    control.index = 2;
    control.descriptor = 0x29ce;
    control.group = MixerGroup::Playback;
    assert!(!control.mute_available());
    control.descriptor = 0x29d1;
    control.group = MixerGroup::Monitoring;
    assert!(control.mute_available());
    control.descriptor = 0x2ac9;
    control.group = MixerGroup::Recording;
    assert!(control.mute_available());
    control.muted = None;
    assert!(!control.mute_available());
}
#[test]
fn boost_waits_for_confirmed_state_and_preserves_eq_draft() {
    let (mut app, events, commands) = app();
    let mut current = X4App::demo().state.unwrap();
    let boost = current.mixer.iter_mut().find(|c| c.index == 10).unwrap();
    assert!(boost.is_microphone_boost());
    boost.microphone_boost = Some(false);
    events
        .send(Event::State(current.clone(), Completion::Refresh))
        .unwrap();
    app.events();
    let _ = app.update(Message::Band(0, 3.0));
    let _ = app.update(Message::Command(Command::MicrophoneBoost(true)));
    assert!(matches!(
        commands.try_recv().unwrap(),
        Command::MicrophoneBoost(true)
    ));
    assert!(app.busy());
    assert_eq!(
        app.state
            .as_ref()
            .unwrap()
            .mixer
            .last()
            .unwrap()
            .microphone_boost,
        Some(false)
    );
    current.mixer.last_mut().unwrap().microphone_boost = Some(true);
    events
        .send(Event::State(current, Completion::Control))
        .unwrap();
    app.events();
    assert!(!app.busy());
    assert_eq!(
        app.state
            .as_ref()
            .unwrap()
            .mixer
            .last()
            .unwrap()
            .microphone_boost,
        Some(true)
    );
    assert!(app.dirty);
    assert_eq!(app.draft.as_ref().unwrap().bands_db[0], 3.0);
}
#[test]
fn progress_stays_on_its_control_until_the_write_is_confirmed() {
    let (mut app, events, _commands) = app();
    events
        .send(Event::State(state(), Completion::Refresh))
        .unwrap();
    app.events();
    app.confirmed = None;
    app.send(Command::Output(Output::Speaker));
    let control = Control::Output(Output::Speaker);
    assert!(app.in_flight(control));
    assert!(!app.in_flight(Control::Output(Output::Headphone)));
    assert_eq!(
        app.pending.as_ref().unwrap().progress,
        "Switching to Speakers…"
    );
    // A background poll is not the write's confirmation.
    events
        .send(Event::State(state(), Completion::Poll))
        .unwrap();
    app.events();
    assert!(app.in_flight(control));
    assert!(app.confirmed.is_none());
    events
        .send(Event::State(state(), Completion::Control))
        .unwrap();
    app.events();
    assert!(!app.busy());
    let confirmed = app.confirmed.as_ref().unwrap();
    assert_eq!(confirmed.control, control);
    assert_eq!(confirmed.message, "Now on Speakers");
}
#[test]
fn confirmation_expires_and_a_failure_never_confirms() {
    let (mut app, events, _commands) = app();
    app.confirmed = Some(Confirmed {
        control: Control::Sxfi,
        message: "Done".into(),
        at: Instant::now().checked_sub(CONFIRMATION).unwrap(),
    });
    let _ = app.update(Message::Tick);
    assert!(app.confirmed.is_none());
    app.send(Command::Refresh);
    assert!(app.busy());
    events.send(Event::Error("Read failed".into())).unwrap();
    app.events();
    assert!(!app.busy());
    assert!(app.confirmed.is_none());
}
#[test]
fn controls_ignore_input_during_a_write_without_looking_disabled() {
    let (mut app, _events, _commands) = app();
    assert!(matches!(
        app.press(true, Message::Revert),
        Some(Message::Revert)
    ));
    assert!(app.press(false, Message::Revert).is_none());
    app.send(Command::Refresh);
    assert!(matches!(
        app.press(true, Message::Revert),
        Some(Message::Ignored)
    ));
}
#[test]
fn only_one_command_is_queued_while_busy() {
    let (mut app, _, commands) = app();
    app.send(Command::Refresh);
    app.send(Command::Feature(Feature::DirectMode, true));
    assert!(matches!(commands.try_recv().unwrap(), Command::Refresh));
    assert!(commands.try_recv().is_err());
}
#[test]
fn failed_operation_discards_unconfirmed_state() {
    let (mut app, events, _) = app();
    events
        .send(Event::State(state(), Completion::Refresh))
        .unwrap();
    app.events();
    let _ = app.update(Message::Band(0, 5.0));
    events.send(Event::Error("Write failed".into())).unwrap();
    app.events();
    assert!(app.state.is_none());
    assert!(app.draft.is_none());
    assert!(!app.busy());
    assert!(app.error.is_some());
}
