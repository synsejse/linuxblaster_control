# Linuxblaster Control desktop

Native Iced controller for Sound Blaster X4. Uses Iced 0.14.0, iced_aw 0.14.1
(number inputs) and [`iced_t3`](https://github.com/synsejse/iced_t3), which provides the theme,
widgets and client-side window decorations. Each point of the band-gain curve sits
above its vertical EQ fader, and a docked bar keeps EQ status and the Apply action
in view while the page scrolls. The backend library remains independent of the GUI.

From `code/` on NixOS:

```sh
nix-shell --run 'cargo run --locked'
# Offline fixture: never opens or discovers hardware, does not read or write user settings files.
nix-shell --run 'cargo run --locked -- --demo'
# Software renderer fallback:
nix-shell --run 'ICED_BACKEND=tiny-skia cargo run --locked -- --demo'
```

`--port`, `--status`, `--profiles` and the binary name `linuxblaster_control` are
unchanged. Demo cannot be combined with device arguments. No new background
service, autostart, host configuration or device reset is installed.

The sidebar switches between EQ and mixer and selects the output and onboard EQ
profile. Output, Direct Mode, gain, SXFI and
profile buttons show confirmed state while a worker applies and reads back changes.
EQ bands and preamp are drafts until Apply EQ. Revert restores confirmed values.
The control being written shows bouncing dots and the title bar names the action;
a check follows once the X4 confirms it. Other controls ignore input until then.
Direct Mode bypasses processing; SXFI requires headphones. The first three profiles
require SXFI Off; the fourth requires On or Battle. Switching processing context
invalidates an old draft. Failed writes clear unconfirmed state, display the error
and reconnect without replaying the failed operation.

Mixer sliders prepare a level; Apply appears and sends it. Mute changes apply
immediately.
Playback, monitoring and recording retain the API's verified labels and ranges.
S/PDIF playback mute is unavailable on the researched firmware.
Microphone Boost in Other controls applies immediately and displays confirmed
device state. It uses the inverted type-63 flag (raw 0 means enabled), with API
readback verification; it is separate from ordinary mute and level controls.

Profile names and active settings come from the X4. The app has no local preset
library, alias mapping, save/load buttons or import/export dialogs. Existing
preset files from older versions are left untouched and ignored. EQ edits stay
in memory until Apply EQ writes them to the selected onboard slot. Closing the
app discards unapplied edits and leaves device settings alone.

Tab/Shift+Tab navigate controls; Enter/Space activate focused buttons. Numeric EQ
fields support keyboard editing. Ctrl+R refreshes the device state. The main pane
scrolls vertically at smaller window sizes; all ten EQ bands fit side by side.

Offline validation and optional native software-rendered captures:

```sh
nix-shell --run 'cargo test -p linuxblaster_control'
nix-shell --run 'X4_UI_CAPTURE_DIR=/tmp/x4-ui cargo test -p linuxblaster_control offline_render_and_state_flows'
```

The rendering harness draws the actual Iced widget tree using tiny-skia at
1120×900, 860×640 and 860×1240 (the whole page at minimum width); it needs neither
a display server nor a device. The fixture
is explicitly illustrative. These tests do not establish hardware behavior or
compositor integration.
