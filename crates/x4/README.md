# soundblaster-x4

Synchronous Rust API for the Creative Sound Blaster X4 (`041e:3278`) control
channel. It talks to the device's CDC ACM serial port. Audio stays with Linux's
`snd-usb-audio`, so no kernel driver, vendor DLL or daemon is needed.

```rust,no_run
use soundblaster_x4::{Feature, Result, SxfiMode, open};

fn main() -> Result<()> {
    let mut x4 = open(None)?; // Or Some("/dev/serial/by-id/...") for a specific X4.
    let state = x4.state()?;
    println!("Firmware {}, output {:?}", state.firmware, state.output);
    if !state.feature(Feature::DirectMode) && state.sxfi.is_some() {
        x4.set_sxfi(SxfiMode::On)?;
    }
    Ok(())
}
```

## Opening a device

`open(None)` finds the single attached X4. If several are connected, pass a
port path; it is checked against the USB VID/PID before opening. The port is
opened exclusively at 115200 baud with DTR/RTS asserted, and the control
channel is unlocked if needed. The caller's environment must grant serial
access, for example through a udev rule.

`X4::connect` accepts any `Read + Write` transport you configure yourself,
such as a recorded fixture in tests. Its reads must time out periodically for
the API's deadlines to work.

## Behaviour you can rely on

- **One owner.** An `X4` serializes every request; share it through a single
  worker thread rather than opening the port twice.
- **Verified writes.** Each high-level setter requires a success ACK and then
  reads the value back, allowing for the firmware's EQ and mixer quantization.
  Setters that trigger asynchronous changes (feature bits, SXFI, EQ slots) poll
  until the device settles.
- **Fail closed.** A timeout, malformed reply, rejected write or readback
  mismatch sets `session.usable` to `false`. Later calls fail until you
  reconnect. Read the state again before another write; never replay a write
  whose outcome is uncertain.
- **Not transactional.** Multi-step operations such as `apply_eq` and
  `restore_mixer_channels` can stop partway. Before each EQ write, `apply_eq`
  re-checks output, slot, SXFI and Direct Mode, so another controller's change
  aborts it instead of writing to the wrong context.
- **Sequential snapshots.** `state()` reads its parts one after another and
  fails if output, slot, SXFI or Direct Mode changed meanwhile. It covers mixer
  masters only; use `capture_mixer_channels` for all 42 levels.
- **Notifications.** Unsolicited frames, such as hardware knob changes, collect
  in `session.notifications` (at most 128) and are never mistaken for replies.

Errors are `Box<dyn Error + Send + Sync>` (`soundblaster_x4::Error`). Timeouts
are `io::ErrorKind::TimedOut`, protocol and validation failures `InvalidData`,
and missing firmware support `Unsupported`.

## API overview

| Area | Methods | Status |
|---|---|---|
| State | `state`, `output`, `features`, `feature`, `feature_support` | live |
| Output | `set_output` | live |
| Feature bits | `set_feature(Feature, bool)` — Direct Mode, headphone high gain, … | live (Direct Mode, gain) |
| Hardware EQ | `eq`, `eq_parameter`, `apply_eq`, `Equalizer`, `Preset` | live |
| EQ slots | `eq_profiles`, `active_eq_profile`, `set_eq_profile`, `stored_eq_profile`, `read_eq_profile` | live |
| Super X-Fi | `sxfi`, `set_sxfi(SxfiMode)` | live |
| Mixer | `mixer`, `set_mixer_level`, `set_mixer_mute`, `mixer_channels`, `set_mixer_channel_raw`, `capture_mixer_channels`, `restore_mixer_channels` | live |
| Microphone Boost | `microphone_boost`, `set_microphone_boost` | live |
| Raw DSP and profile data | `dsp` module: `dsp_read`, `dsp_write`, `write_profile_data`, `write_profile_field` | static |
| SXFI selectors | `sxfi` module: `sxfi_query`, `set_sxfi_token`, `set_sxfi_button`, numeric helpers | static |
| Device commands | `controls` module: device info, filters, prompts, LEDs, resets | static, some destructive |
| Transfers | `transfer` module: personalization upload/download | experimental |
| Host helpers | `software` module: percent tables, bass/treble curves | host-only |

*Live* means the API has been verified on hardware with write, readback and
restore. *Static* means it was derived from firmware or vendor-binary analysis
and is covered by scripted tests only.

Device rules the API enforces:

- Super X-Fi needs headphone output and Direct Mode off.
- EQ slots 0–2 (Music, Movie, Footsteps) need SXFI off. Slot 3 (SXFI) is
  active only while SXFI is on. Selecting a slot never toggles SXFI.
- Direct Mode bypasses EQ and SXFI, so EQ and slot writes are refused while it
  is on.
- S/PDIF playback has no independent mute, so `set_mixer_mute(2, _)` is
  rejected.
- EQ gains are limited to −12..12 dB.

The [protocol reference](docs/PROTOCOL.md) documents every command's bytes,
firmware quirks and what remains unverified.

## Testing

```sh
cargo test -p soundblaster-x4
cargo doc -p soundblaster-x4 --no-deps
# Read-only state dump from real hardware:
cargo run -p soundblaster-x4 --example dump_state -- /dev/ttyACM0
```

Tests use captured traffic in `tests/fixtures` and scripted fakes; they never
open hardware. `examples/hardware_check.rs` exercises live writes. It refuses to
run without `--exercise-and-restore` and an unused `X4_CHECK_BACKUP` path. It
backs up the EQ slots, processing state and mixer before changing anything,
then restores them.
