# Linuxblaster Control desktop

Native Iced controller for Sound Blaster X4. Uses Iced 0.14.0, iced_aw 0.14.1
(number inputs) and [`iced_t3`](https://github.com/synsejse/iced_t3), which provides the theme,
widgets and client-side window decorations. Each point of the band-gain curve sits
above its vertical EQ fader, and a docked bar keeps EQ status and the Apply action
in view while the page scrolls. The backend library remains independent of the GUI.

The `iced_t3` submodule is pinned to `c9be75b`. Sidebar controls use its named
configuration structs and tile-size enum; application and window setup share the
`linuxblaster-control` desktop ID through its setup helpers.

From `code/` on NixOS:

```sh
nix-shell --run 'cargo run --locked'
# Offline fixture: never opens or discovers hardware, does not read or write user settings files.
nix-shell --run 'cargo run --locked -- --demo'
# Software renderer fallback:
nix-shell --run 'ICED_BACKEND=tiny-skia cargo run --locked -- --demo'
```

Demo cannot be combined with device arguments. The app installs no background
service or autostart entry.

The sidebar navigates Equalizer, Mixer and Device. Device groups Output,
Super X-Fi, Direct Mode and headphone gain into settings rows. The onboard EQ
profile selector sits above the curve on Equalizer. Every device control is a
draft: output, Direct Mode, gain, Super X-Fi,
profile, EQ, mixer level, mute and Microphone Boost. The docked **Apply** bar is
visible on any page only while edits are pending or being applied, and sends all
pending settings together. It disappears after Apply or Revert; a brief titlebar
message confirms completion. **Revert** restores confirmed values; background
reads and page changes keep pending edits.

The full batch is validated before its first write and stops on failure. Output,
mode and profile changes can load a new EQ curve, so Apply those selections before
editing their EQ. Selecting a new context discards the old EQ draft with a notice.
Direct Mode bypasses processing; SXFI requires headphones. The first three profiles
require SXFI Off; the fourth requires On or Battle.

Failed writes clear unconfirmed state, display the error and reconnect without
replaying the failed operation. Playback, monitoring and recording retain the
API's per-control limits. SPDIF Out playback mute is unavailable on the researched
firmware. Microphone Boost uses the inverted type-63 flag (raw 0 means enabled),
separate from ordinary mute and level controls.

Profile names and active settings come from the X4. The app keeps no presets.
Closing it discards unapplied edits and leaves device settings alone.

Tab/Shift+Tab navigate controls; Enter/Space activate focused buttons. Numeric EQ
fields support keyboard editing. Ctrl+R refreshes the device state. The window opens at 1120×780, matching razerr-rs, and resizes down to 820×560.
The main pane scrolls at smaller sizes; all ten EQ bands fit side by side.

Offline validation and optional native software-rendered captures:

```sh
nix-shell --run 'cargo test -p linuxblaster_control'
nix-shell --run 'X4_UI_CAPTURE_DIR=/tmp/x4-ui cargo test -p linuxblaster_control offline_render_and_state_flows'
```

The rendering harness draws the actual Iced widget tree using tiny-skia at
1120×780, 820×560 and 820×1240 (the whole page at minimum width); it needs neither
a display server nor a device. The fixture
is explicitly illustrative. These tests do not establish hardware behavior or
compositor integration.
