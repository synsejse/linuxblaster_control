# Sound Blaster X4 control protocol

Wire reference for the `soundblaster-x4` crate. Target: Sound Blaster X4 / SB1815
(`041e:3278`), firmware `1.7.250324.0910`. Other firmware may differ.

Conventions: bytes are hexadecimal, multi-byte integers little endian. Tables
list **payloads** (after the frame header) unless they show a complete frame.
`[]` is an empty payload.

Evidence levels used below:

- **Live** – exercised against a real X4 with write, readback and restore.
- **Static** – derived from Creative's binaries or firmware analysis, covered by
  scripted tests only.

## Transport

The X4 exposes a CDC ACM interface (`/dev/ttyACM*` through `cdc_acm`) next to its
UAC2 audio interfaces, which stay with `snd-usb-audio`. `open()` opens the port
exclusively at 115200 8N1 with DTR and RTS asserted. The CDC stream is a byte
stream: one read can hold a partial frame, several frames, or notifications
interleaved with replies.

## Framing

```text
5a command:u8 length:u8    payload
5b command:u8 length:u16le payload
```

Lengths exclude the header; there is no checksum. The parser accepts both
markers, skips bytes before a marker and rejects payloads over 4096 bytes. The
host always sends `5a`. Replies can exceed the outgoing limit (for example a
65-byte SXFI reply).

### Acknowledgements

Command `02` acknowledges a SET:

```text
original_command:u8 status:u8 associated_data[8]
```

| Status | Meaning |
|---|---|
| `00` | success |
| `01` | pending (only as a 4-byte ACK; a final ACK follows) |
| `02` | conditional success |
| `80` | general failure |
| `81` | unsupported |
| `82` | temporarily unsupported |
| `83` | invalid parameter |
| `84` | invalid length |
| `85` | booting |

`Session::set` accepts only a ten-byte status-`00` ACK. A query (`Session::get`)
that receives any ACK for its command fails, because a query must return data.
Frames carry no sequence number, so the API correlates replies by command and
selector. Unmatched frames go to `Session::notifications` (bounded to 128)
and never satisfy a later request.

An ACK proves the request was accepted, not that state changed. Every high-level
setter reads the value back. A timeout, malformed reply, rejected write or
readback mismatch sets `Session::usable` to false. Reconnect and read the state
before writing again. Never replay an uncertain write.

## Session start

1. Probe `03 []`. A reply `max_payload:u16` means binary mode is ready; the
   tested firmware returns `3b 00` (59). The API accepts 8..255.
2. On timeout, write `\r\nwhoareyou.MyApp8\r\n`. The reply is exactly 47 bytes:
   `whoareyou`, four seed bytes, 32 challenge bytes and CRLF. The challenge can
   contain CRLF, so it is read by length, not by line. `NotYet\r\n` means the
   device is booting; the API retries three times at one-second intervals.
   `Unknown command\r\n` means no unlock is needed.
3. Send `unlock || random[16] || ciphertext[32] || tag[16] || CRLF` and wait
   for `unlock_OK\r\n`:

   ```text
   key       = seed[0..2] || fixed[28] || seed[2..4]
   fixed     = d3 1a 21 27 9b e3 46 f0 99 9d 6e c4 c3 fe
               be 98 90 18 69 c1 18 fb b1 25 6e 0c e0 7b
   cipher    = AES-256-GCM, nonce random[0..12], empty AAD, 16-byte tag
   plaintext = challenge[32]
   ```

   The seed equals the little-endian VID/PID (`1e 04 78 32`). Use fresh
   randomness for every challenge.
4. Send `SW_MODE1\r\n`; expect a status-`00` ACK for command `6d`. Probe `03`
   again.
5. Read firmware with `09 [02]` → `02 length bytes...` (UTF-8, trailing NUL
   removed).

## Output and feature bits — live

| Command | GET → reply | SET |
|---|---|---|
| `2c` output | `01` → `01 output:u32` | `00 output:u32` |
| `2c` supported outputs | `02` → `02 flags [output:u32]...` | — |
| `39` support | `02` → `02 supported:u32 reset_required:u32` | — |
| `39` state | `01 ff ff ff ff` → `01 mask:u32` | `00 bit enabled` |

Output values: speaker `1`, headphone `4`. In the support reply, bit 7 of
`flags` must be set and its low seven bits count the outputs.

Feature masks are 32-bit. The SET takes a **bit number**, not a mask.
`Feature` names the established bits:

| Bit | `Feature` | Vendor name | Status |
|---|---|---|---|
| 5 | `DirectMode` | DirectMode | live; bypasses EQ and SXFI |
| 6 | `HeadphoneHighGain` | HPHighGain | live |
| 15 | `SpdifPassthrough` | SPDIFPassthruMode | static |
| 20 | `SaveHeadphoneHighGain` | SaveHPHighGain | static |
| 21 | `HeadphoneVirtualization` | HPVirtualization | static |
| 26 | `Uac2` | EnableUAC2 | static |

`set_feature` rejects unsupported or reset-required bits, then polls readback
for up to three seconds because state can settle after the ACK.

## Hardware EQ — live

| Command | Request → reply |
|---|---|
| `10` DSP capability | `[]` → 8 bytes; byte 0 bit 6 advertises EQ |
| `11` read | `01 96 parameter` → `01 00 96 parameter value:f32` |
| `12` write | `01 96 parameter value:f32` → ACK |

Module `96` parameters: `09` enable (`0.0`/`1.0`), `0a` preamp, `0b..14` the
ten bands. UI labels are 31, 62, 125, 250, 500 Hz, 1, 2, 4, 8 and 16 kHz; the
acoustic centre frequencies are unmeasured. The API limits gains to −12..12 dB
(`15 [00]` reports +12/−12/1). Readback is compared within 0.011 dB.

Writing a parameter updates the **active stored slot** immediately; no separate
save command exists. Edits survive switching slots; persistence across power
loss is untested. `apply_eq` writes only changed values. Before each write it
re-checks output, active slot, SXFI and Direct Mode. It is a write sequence,
not a transaction.

Generic `dsp_read`/`dsp_write` take up to 28 parameters per read and 9 tuples per
write frame. A read reply is `count more (module parameter value:u32)...`;
`13` reads defaults. The firmware rejects command `14`, so the crate does not
expose it.

## EQ slots — live

| Command | GET → reply | SET |
|---|---|---|
| `1a` active slot | `01 02 00` → `02 raw_index` | `00 02 index` |
| `1d` inventory | `01 02 flags` → `01 00 02 flags count count [info:4]...` | `00 02 fields[4]` |
| `18` field | `01 02 index` (name) / `04 02 index` (aux) | `00`/`03 02 index ...` |
| `17` stored EQ | `01 02 index flags` → blocks | `00 02 index flags total:u16 block data` |

Slots: 0 Music, 1 Movie, 2 Footsteps Enhancer, 3 SXFI. Slots 0–2 require SXFI
off. Slot 3 becomes active when SXFI is enabled; selecting it with SXFI off
returns status `80`. `set_eq_profile` enforces this and never toggles SXFI
itself. After the ACK the device loads the slot asynchronously. Early reads can
show intermediate values, so the API waits until active and stored EQ agree.

The active-index reply masks bit 7. That bit copies cached record byte 35 and
is not an EQ enable; EQ enable is always `96:09`.

Profile `flags`: bit 0 selects factory data; bits 1–3 select the bank: speaker
`0`, headphone `1`, current `7` (so `00`, `02`, `0e`, plus `1` for factory).
`read_eq_profile(ProfileBank, ProfileSource, index)` covers every combination.

Stored-EQ reply blocks: `01 02 index flags total_size:u16 block:u8 data`. Block
bits 0–6 are the sequence number; bit 7 means more blocks follow. The assembled
76-byte body is `count:u32 = 12` followed by twelve `96 parameter value:f32`
records covering `09..14` exactly once. A writable read reports the global EQ
enable in `09`, even for an inactive slot.

Field replies: `selector 02 index total|more count offset [char:2]...`. Names
hold up to 32 characters, auxiliary fields up to 8. The firmware stores only
low bytes and zero-extends them on the wire; writes change both banks.
Non-final field-write blocks are acknowledged with status `80`, which is
expected. Slot names come from the device. Creative app preset labels (e.g.
"Flat") are app-local and may differ.

## Super X-Fi — live

| Command | GET → reply | SET |
|---|---|---|
| `26` buttons | `05` → `05 00 count 00 [id:u16]...` | — |
| `26` button state | `08 ff ff` → `08 ff ff state:u64` | `07 id:u16 enabled:u16` |
| `6f` supported modes | `03` → `03 count [token:4]...` | — |
| `6f` current mode | `0d` → `0d token:4 layout:6` | `0c token:4` |

SXFI enable is button ID 30 (`1e 00`). Its state bit is its **position** in the
button inventory (bit 0 on the X4), not bit 30. Mode tokens are `MV  ` (On),
`PCG ` (Battle), plus `FPS ` and `MSC ` (spaces included):

| `SxfiMode` | Button | Token |
|---|---|---|
| `Off` | 0 | unchanged (the latent token stays selected) |
| `On` | 1 | `MV  ` |
| `Battle` | 1 | `PCG ` |

A latent `PCG ` with the button off still means Off. Selecting a token first
returns a pending ACK `6f 01 0a 00`, then a final ACK `6f 00 0c 00...`. The
pending ACK carries `0a`, so `Session::set_pending` filters only the final ACK
on `0c` and allows ten seconds overall.

`set_sxfi` requires headphone output and Direct Mode off. It selects the token,
writes the button, then waits for mode, slot and EQ to settle. Lower-level
`set_sxfi_token` and `set_sxfi_button` skip those context checks.

Other `6f` selectors are exposed numerically through `sxfi_query` and helpers
whose semantics are unresolved:

| Selector | API | Reply / note |
|---|---|---|
| `00`, `01` | `SxfiQuery::Capabilities`, `Selectors` | 8 bytes / variable |
| `02` | `sxfi_configure_modes_02` | `02 count token[4]...` → ACK |
| `05`, `06`, `08` | `State`, `Identifiers`, `Identifier` | 2 / 65 / 33 bytes; `Debug` redacted |
| `07` | `sxfi_action_07` | ACK |
| `09`, `0a` | `State09` / `set_sxfi_flag_0a` | 3 bytes / `0a bool` |
| `0b`, `16` | `sxfi_value_0b`, `sxfi_value_16` | `selector count data` → 3 bytes |
| `0e`, `0f`, `10` | `Capability0e`, `Flag0f`, `State10` | 6 / 3 / 2 bytes |
| `11` | `set_sxfi_flag_11` | `11 bool` |
| `12`, `13` | `Capability12`, `Entries13` | `2+N` / `2+7N` bytes |
| `14` | `sxfi_compound_14` | `14 fields[7] bool` |
| `15` | `sxfi_result_15` | `15 key[7]` → 9 bytes |

## Mixer — live

| Command | Request | Reply |
|---|---|---|
| `21` descriptors | `00` | `count count 00 00 [descriptor:u16]... (+4 unknown bytes)` |
| `22` range | `01 index` + 31 zero bytes | `01 00 index max:i16 min:i16 step:u16` |
| `23` channel GET | `01 index 00 00 channel` | `index level:i16 channel` |
| `23` all channels | `01 index 00 00 ff` | `index [level:i16 channel]...` |
| `23` channel SET | `00 index level:i16 channel` | ACK |
| `24` flag GET | `01 index` | `index flag` |
| `24` flag SET | `00 index flag` | ACK |

Descriptor bits: type `003f`, volume `0040`, flag/mute `0080`, source `0100`,
recording `0200`, custom name `0400`, dB format `0800`; the upper nibble counts
channels. Levels are signed Q8.8 dB (`raw / 256`).

**Always send the channel byte**, even for master channel 0. Short requests
make the firmware read a stale receive-buffer byte. Channel 0 is master;
1–8 are left, right, centre, subwoofer, rear L/R and side L/R. Indices 0–1 have
9 channels, indices 2–9 have 3: 42 levels in total
(`capture_mixer_channels`). A master write shifts lookup steps with clipping,
so `restore_mixer_channels` validates the whole snapshot, then writes all
masters before individual channels.

Observed X4 layout (the API derives identity from the descriptor type, not the
index):

| Index | Descriptor | Control |
|---|---|---|
| 0 | `89c1` | Speakers (playback) |
| 1 | `89c2` | Headphones (playback) |
| 2 | `29ce` | S/PDIF output (playback) |
| 3 | `29cf` | Microphone (monitoring) |
| 4 | `29d0` | Line-in (monitoring) |
| 5 | `2ac3` | Microphone (recording) |
| 6 | `2ac4` | Line-in (recording) |
| 7 | `2ac5` | What U Hear (recording) |
| 8 | `29d1` | S/PDIF (monitoring) |
| 9 | `2ac9` | S/PDIF (recording) |
| 10 | `00bf` | Microphone Boost (type 63) |

Firmware quirks:

- Level readback quantizes differently from the advertised step (up to 101 raw
  units against a step of 64). Setters accept differences up to
  `max(step_raw, 128)`.
- Playback mutes for indices 0 and 1 are linked. Writing index 2 (S/PDIF
  output) changes them but its own flag stays false, so `set_mixer_mute`
  rejects index 2.
- Type 63 is Microphone Boost with an **inverted** flag: raw `0` means enabled.
  `MixerControl::microphone_boost` and `set_microphone_boost` use the enabled
  value.
- SXFI or slot changes can clear monitoring mutes; restore the mixer last.

## Other commands — static

These methods send the documented bytes but do not interpret them; many are
destructive or have unresolved meanings.

| Cmd | API | Payload | Note |
|---|---|---|---|
| `01` | `protocol_version` | `00` → 2 bytes | |
| `05`, `20`, `30`, `90` | `capability_word` | `[]` (`90`: `00`) → u32 | numeric masks |
| `09` | `device_information` | `00`..`06` | may contain serials; `Debug` redacted |
| `0b` | `protocol_format` | `[]` → 1 byte | |
| `0c` | `connection_mode`, `set_connection_mode_2`, `reset_connection` | `01` / `02` / `00` | the query may invoke the setter; reset is destructive |
| `37` | `low_level_read` / `low_level_write` | `80 id addr 00 00 00 01` / `00 id 00 addr value` | IDs `18`, `4c`, `30` (SET only) |
| `3a` | `set_led_word` | `0a 00 03 08 reserved word:u32` | colour order unresolved |
| `52` | `reset_actions` / `reset_action` | `02` / `00 action 00` | destructive |
| `53` | `upgrade_inventory`, `request_upgrade`, `upgrade_context` | `02` / `00 01` / `01 word:u32` | not a flashing workflow |
| `54` | `echo` | `00 data` → identical | |
| `6c` | `filter_inventory`, `filter_selection`, `set_filter_selection` | `02` / `01` / `00 id:u16` | cached selector only |
| `6d` | `operation_mode` / `set_operation_mode` | `01` / `00 01` | |
| `70` | prompt language methods | `00`, `02`, `03` / `01 lang[2]` | `en`, `zh`, `ja` |
| `72` | transport option methods | `00`, `01` / `02 bool` | meaning unresolved |
| `9b` | `factory_reset` | `01` | destructive |

Commands `29`/`4b` (speaker configuration) and `14` have no X4 handler and are
not exposed.

## Transfers (`98`) — experimental, static

`transfer` implements bounded personalization uploads and asset downloads
derived from firmware analysis. It is **not validated on hardware**.

| Payload | Meaning |
|---|---|
| `02 total:u32 type` | start; high type bit requests a download |
| `02 offset:u32 token:u32 flags packet:u16` | `StartReply`; slots `(flags & 1f) + 1`, resume `flags & 80` |
| `03 total offset token slots resume packet:u16 kind [crc:u32]` | `TransferInfo` |
| `04 seq count data` | data packet (`count` is u16 in `5b` frames) |
| `04 status missing:u32 committed:u32` | `WindowReply` |
| `05 committed:u32` | window shift |
| `06 80 last_slot` | final-window marker |
| `09 type token:u32` → `09 type status token result 00 00 00` | `ProcessingResult` |

- **Uploads:** `upload_prepared` sends a fresh extended-frame transfer with an
  IEEE CRC32. It retransmits only missing slots, shifts windows, sends the
  final marker, then polls processing (`ProcessingPolicy`, 15 s by default).
  `upload_prepared_with_progress` reports checkpoints and supports local
  cancellation; no wire cancel exists. `resume_prepared` needs the same asset,
  a matching checkpoint and a device resume offer.
- **Upload outcomes:** `Processed` means the firmware finished processing, not
  that sound changed. `AlreadyPresent` is validator status `10`.
  `UploadKind::FirmwareStage` (type `01`) reports staging only; it never
  applies or flashes firmware.
- **Downloads:** `download_asset` handles multi-window downloads with bounded
  missing-slot recovery. Bounds are 16 MiB, 32 slots and 60 s. It validates
  CRC before sanitizing room descriptors and never returns partial data.
  Download resume is not supported.

## Host-side helpers

`software::PercentTable` holds Creative's seven percent-to-dB lookup tables.
`tone_curve` and `adjust_tone` reproduce the app's bass/treble adjustment of
the ten EQ bands. Both are host calculations, not device commands.

## Not established

Acoustic effects, centre frequencies, power-loss persistence, cold power-on,
suspend/resume and PCM formats are unverified. SmartComms, microphone DSP,
Scout Mode and SXFI head-map uploads are not implemented as settings.

