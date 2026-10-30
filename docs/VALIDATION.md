# X4 adaptation validation

Validated on 2026-10-30, NixOS x86_64, with a connected Sound Blaster X4
(`041e:3278`, firmware `1.7.250324.0910`).

## Automated

- `cargo test --locked --all-targets`: 15 tests pass (13 protocol/device tests,
  2 CLI/headless GUI tests).
- `cargo clippy --locked --all-targets -- -D warnings`: passes.
- `cargo fmt --check`: passes.
- `nix build`: passes, including the package test phase. The updated Nix package
  launches on this host; the inherited 2025 pins were updated for the current graphics stack.
- The NixOS module evaluates with `hardware.soundblaster-x4.enable = true` and
  includes the X4 package in `services.udev.packages`.
- Recorded Windows traffic verifies Rust AES-GCM output, both frame formats,
  every split boundary, EQ decoding and multi-block profile assembly.
- Simulated cold initialization includes binary challenge bytes containing CRLF.
- Tests check failed/malformed ACKs, timeout invalidation/no retries, unrelated
  notifications, pre-write validation, output guard and independent setter readback.

## Live Linux reads

User-authorized `pkexec setfacl` granted the current user access to `/dev/ttyACM0`.
This is a temporary device-node ACL, cleared when the device is removed. No NixOS
configuration, audio driver or permanent system rule was changed during testing.

- `--status` read firmware, capability masks, headphone output, feature state and EQ.
- Preamp was -9 dB; the first bands were approximately 6.76, 9, 7.62 and 3.14 dB.
- `--profiles` assembled both speaker and headphone profiles. Speaker EQ was flat;
  headphone EQ matched the active settings.
- JSON outputs are in `tests/fixtures/linux-status.json` and `linux-profiles.json`.
- The native GUI opened on Wayland and displayed those live values; its layout was
  visually inspected. A second process was refused with `Device or resource busy`
  while the GUI owned the port.

No audio-setting changes were sent during these Linux checks. Cold power-on was
not forced, so live reads do not prove cold unlock behavior. No claim is made about
PCM playback, recording, rates, channels, acoustic EQ response, power-loss persistence,
suspend/resume, output changes, Direct Mode or gain changes on Linux.

## Remaining validation

- Exercise the Rust cold unlock after a physical power cycle.
- Exercise unplug/replug and suspend/resume, including a pending transaction.
- Compare each supported setter's readback with the physical/acoustic result.
- Establish EQ frequencies, actual limits, quantization and persistence.
- Validate desktop file dialogs, preset application and packaging on other distributions.
- SXFI, stored-profile writes, mixer writes and Windows host effects remain out of scope.
