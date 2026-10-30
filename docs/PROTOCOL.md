> Research snapshot from the sibling `research/` bundle. References to the Python CLI and evidence files describe that bundle; see [VALIDATION.md](VALIDATION.md) for this Rust fork.

# Sound Blaster X4 USB control protocol

Investigated 2026-10-30 on the physically connected X4. Evidence labels below:

- **Live**: reproduced against this device, with raw exchange saved.
- **Static**: recovered from the signed Creative binaries, without testing that operation here.
- **External**: reported by the linked independent project.
- **Inference**: plausible interpretation requiring a further experiment.

## 1. Identity and USB topology — live

| Property | Value |
|---|---|
| Product / model | Sound Blaster X4 / SB1815 |
| Internal product name | Accent2 (static vendor constants) |
| VID / PID | `041e:3278` |
| bcdUSB / bcdDevice | `0x0200` / `0x1070` |
| Firmware string | `1.7.250324.0910` |
| CDC port on this machine | `COM3` |
| Windows audio driver | Microsoft `usbaudio2.inf` / `usbaudio2` |
| Windows CDC driver | Microsoft `usbser.inf` / `usbser` |

Device descriptor:

```text
12 01 00 02 ef 02 01 40 1e 04 78 32 70 10 01 02 09 01
```

| Interface | Function | Endpoints observed |
|---|---|---|
| 0 | HID consumer control | interrupt IN `86`, max 64 |
| 1 | CDC ACM control | interrupt IN `87`, max 8 |
| 2 | CDC data | bulk OUT `03`, max 512; bulk IN `82`, max 256 |
| 3 | UAC2 audio control | interrupt IN `84`, max 6 |
| 4 | UAC2 primary playback | isochronous OUT `01`, feedback IN `83` |
| 5 | UAC2 recording | isochronous IN `81` |
| 6 | UAC2 secondary playback | isochronous OUT `02`, feedback IN `85` |

Interface 4 advertises 2-, 6-, and 8-channel PCM alternatives, each in 16-bit/2-byte and 24-bit/3-byte containers. Interface 5 advertises stereo 16- and 24-bit recording. Interface 6 includes stereo PCM alternatives and an additional format alternative not decoded by the descriptor summary script. These are descriptors, not evidence that every format/rate combination was successfully streamed. UAC2 sample rates require clock-control queries; they were not established here.

Full raw configuration: `evidence/usb-configuration.bin`; decoded interfaces: `evidence/usb-interfaces.json`. Host hub metadata was read using standard Windows USB IOCTLs. No audio driver was replaced.

## 2. Serial transport and session — live

Use CDC ACM with 115200 baud, 8 data bits, no parity, one stop bit; assert DTR. The native CTCDC setup uses `EV_RXCHAR | EV_TXEMPTY`, preserves unrelated existing DCB flags, clears DTR/RTS mode bits, then issues `SETDTR`. The first successful .NET probe had DTR and RTS asserted. A 9600-baud identity probe also worked; that does not establish that line coding is universally ignored.

The Creative App's passthrough layer ultimately writes the raw command bytes to the CDC serial stream. There is no additional libusb/vendor-request wrapper around a `5A` command. On Linux, `cdc_acm` transports these bytes through the bulk endpoint pair while `snd-usb-audio` continues to handle audio.

### Ready-state probe

```text
TX  5a 03 00
RX  5a 03 02 3b 00
```

`0x003b` means **59 bytes maximum command payload**, not firmware version. This query returning a valid response proves binary readiness. If ready, skip unlock and software-mode switching.

### Unlock when the channel is not ready

```text
TX  ASCII "whoareyou.MyApp8\r\n"
RX  ASCII "whoareyou" || seed[4] || challenge[32] || CRLF
TX  ASCII "unlock" || random[16] || ciphertext[32] || tag[16] || CRLF
RX  ASCII "unlock_OK\r\n"
TX  ASCII "SW_MODE1\r\n"
RX  5b 02 0a 00 6d 00 00 00 00 00 00 00 00 00
TX  5a 03 00
RX  5a 03 02 3b 00
```

The challenge reply is exactly **47 bytes**. The random challenge can itself contain CRLF; do not use a delimiter-only line parser. `NotYet\r\n` is a boot-state response documented in the vendor native path. `Unknown command\r\n` is treated there as an older-firmware path requiring no crypto response. Neither special response was seen on this X4.

Crypto:

```text
seed on this X4 = 1e 04 78 32
key = seed[0:2] || fixed_middle[28] || seed[2:4]
fixed_middle = d3 1a 21 27 9b e3 46 f0 99 9d 6e c4 c3 fe
               be 98 90 18 69 c1 18 fb b1 25 6e 0c e0 7b
AES-256-GCM nonce = random[0:12]
plaintext = challenge[32]
AAD = empty
tag length = 16
```

Generate a fresh 16-byte random field each time. All 16 bytes are transmitted, but the first 12 are the GCM nonce. The four seed bytes coincide with little-endian VID/PID. Do not hardcode the X3 seed: X3 is `041e:3264`, this X4 is `041e:3278`.

The template key was located independently at **file offset `0x1d9874`, RVA `0x1dba74`, VA `0x101dba74`** in the exact `CTCDC.dll` identified in `evidence/provenance.json`. Older X3 documentation's VA `0x101d9a74` is not the right address for this binary. The full template key's SHA-256 is `d9209f4b037d4d4323df8999c681124e3c91a02a201a1ea417a31ef764fd5ef2`.

Hardware proof is in `evidence/powershell-session-probe.json`. The first Python cold-start probes returned no data; the .NET probe then completed the handshake. Warm Python control works. The final CLI combines a CRLF prefix and greeting in one fallback write and strictly checks readiness; this cold-start path still needs a real power-cycle test. The exact reason for the earlier Python/.NET difference was not established.

### Post-session queries

```text
Firmware TX  5a 09 01 02
Firmware RX  5a 09 12 02 10 31 2e 37 2e 32 35 30 33 32 34 2e 30 39 31 30 00
Buttons TX   5a 26 01 05
Buttons RX   5a 26 06 05 00 01 00 1e 00
```

Firmware payload: selector `02`, byte length `10`, 16 bytes containing the NUL-terminated string. Button capability data includes one button ID `0x001e`, which the managed enum identifies as SXFI on/off.

## 3. Binary framing and acknowledgements — live + static

```text
5A  command:u8  payload_length:u8    payload
5B  command:u8  payload_length:u16le payload
```

No checksum appears in these frames. `5B` has a **two-byte** length. `SW_MODE1` returns a `5B` ACK; ordinary observed queries return `5A`. Do not interpret the high length byte as a payload byte.

CDC is a byte stream. One read can contain a partial frame, several frames, or asynchronous notifications interleaved with query responses. A single USB bulk transfer is not a protocol message boundary.

Command `02` is an ACK. Observed payload length is 10:

```text
original_command:u8  status:u8  associated_data[8]
```

Status codes recovered from the vendor enum:

| Code | Meaning |
|---|---|
| `00` | success |
| `01` | data pending |
| `02` | conditional success |
| `80` | general failure |
| `81` | unsupported |
| `82` | temporarily unsupported |
| `83` | invalid parameter |
| `84` | invalid length |
| `85` | booting |

The CLI accepts only `00` for setting changes, then queries the value. A successful OS write alone is not confirmation. There is no sequence number in the observed framing; serialize requests and correlate by command plus selector/parameter. After a timeout, do not blindly repeat a state-changing command.

## 4. General feature flags — getters live, setters static

```text
Support TX  5a 39 01 02
Support RX  5a 39 09 02 60 80 30 04 00 00 00 00
State TX    5a 39 05 01 ff ff ff ff
State RX    5a 39 05 01 40 00 10 04
```

Support payload: `02 || supported_mask:u32le || reset_needed_mask:u32le`.
State query: `01 || requested_mask:u32le`; response: `01 || state_mask:u32le`.
Setter: `00 || feature_bit_position:u8 || enabled:u8`.

Observed supported mask `0x04308060`; state `0x04100040`; reset mask zero.

| Bit | Name from vendor binary | State observed | Setter format |
|---|---|---|---|
| 5 | DirectMode | off | `5a 39 03 00 05 vv` |
| 6 | HPHighGain | on | `5a 39 03 00 06 vv` |
| 15 | SPDIFPassthruMode | off | `5a 39 03 00 0f vv` |
| 20 | SaveHPHighGain | on | `5a 39 03 00 14 vv` |
| 21 | HPVirtualization | off | `5a 39 03 00 15 vv` |
| 26 | EnableUAC2 | on | `5a 39 03 00 1a vv` |

`vv` is `00` or `01`. Feature names are from the managed binary; their acoustic effects were not measured. The included CLI exposes Direct Mode and high gain; it deliberately has no UAC-mode setter. The external X4 ARM64 research verifies Direct Mode `00/01` transitions on another X4. No Direct Mode or gain transition was performed here.

## 5. Output routing — getters live, setter static/external

```text
Get TX        5a 2c 01 01
Get RX        5a 2c 05 01 04 00 00 00
Support TX    5a 2c 01 02
Support RX    5a 2c 0a 02 82 01 00 00 00 04 00 00 00
Set speaker   5a 2c 05 00 01 00 00 00
Set headphone 5a 2c 05 00 04 00 00 00
```

Support reply: operation `02`, final/count byte `82` (two targets, high bit set), followed by two little-endian u32 targets: speaker `1`, headphone `4`. Current output was headphone. Set payload uses operation `00`, response/notification uses `01`. This setter was traced in the app and agrees with X3 research, but routing was not switched on this device during this session.

## 6. Malcolm DSP parameters and hardware EQ — live

```text
Support TX  5a 10 00
Support RX  5a 10 08 40 00 00 00 00 00 00 00
```

The first u32 has only bit `0x40`, named `GraphicEQ` in the vendor enum. The X4 did not reply to the tested legacy surround, dialog, smart-volume, crystalizer, or Xbass parameter reads. Thus those legacy hardware controls cannot be assumed available merely because they exist in the shared library. This does not prove that all similarly named Creative App features are absent: some are host APO effects or use other paths.

Generic get and set:

```text
GET  5a 11 03 01 module parameter
SET  5a 12 07 01 module parameter value:f32le
RX   5a 11 08 01 00 module parameter value:f32le
```

`01` is parameter count; the second response byte is an additional-packet indicator. A parameter tuple is module byte, parameter byte, IEEE-754 float32 in little-endian order. For X4 hardware EQ, module is **`96`**.

| Parameter | Meaning | Value before and after tests |
|---|---|---|
| `09` | EQ enabled | `1.0` |
| `0a` | preamp dB | `-9.0` |
| `0b` | band 0 gain | approximately `6.76` |
| `0c` | band 1 gain | `9.0` |
| `0d` | band 2 gain | approximately `7.62` |
| `0e` | band 3 gain | approximately `3.14` |
| `0f` | band 4 gain | approximately `0.97` |
| `10` | band 5 gain | approximately `0.29` |
| `11`–`14` | bands 6–9 | `0.0` |

Validated experiment:

```text
GET preamp       5a 11 03 01 96 0a
RX original      5a 11 08 01 00 96 0a 00 00 10 c1    # -9.0
SET -9.5         5a 12 07 01 96 0a 00 00 18 c1
RX ACK           5a 02 0a 12 00 00 00 00 00 00 00 00 00
GET/readback     ... 00 00 18 c1                      # -9.5
RESTORE -9       5a 12 07 01 96 0a 00 00 10 c1
RX ACK           5a 02 0a 12 00 00 00 00 00 00 00 00 00
GET/readback     ... 00 00 10 c1                      # -9.0
```

The test verified protocol state changes, not an acoustic measurement. Center frequencies, exact hardware limits, quantization, and persistence through power loss remain untested.

## 7. Separate speaker/headphone profiles — live + static

```text
Active profile  5a 1a 03 01 02 00  ->  5a 1a 02 02 00
Speaker profile 5a 17 04 01 02 00 00
Headphone       5a 17 04 01 02 00 02
```

The final `00`/`02` is **output genre**, not page number: vendor `RawCmdMalcolmProfileGet` contains `Operation, Type, Index, GenreMask`. Type `02` is device profile, index `00` is profile zero, genre `00` is speaker and `02` headphone. Older X3 notes label these as pages; the managed type and differing X4 EQ data support the genre interpretation.

Response payload:

```text
operation:u8 type:u8 index:u8 genre:u8 total_size:u16le block:u8 data...
```

`block & 0x7f` is the block index; bit `0x80` means more blocks follow. Match all four identity fields, validate block order and total size, and concatenate only data after the seven-byte prefix.

Both observed profiles contain 76 data bytes split over two frames (payload lengths 59 and 31). The assembled data is `parameter_count:u32le` followed by 12 six-byte parameter tuples. Speaker profile zero is enabled with flat EQ/preamp; headphone profile zero contains the custom settings above. `x4ctl.py profiles` reads both without modifying them. Stored profile writes were not attempted.

## 8. Mixer controls — live reads, static write layouts

Capability request: `5a 21 01 00`.
Response begins `total:u8 count:u8 continuation:u8 first_index:u8`, then `count` u16le descriptors. This X4 returned 11 descriptors plus four trailing bytes whose role was not established.

Descriptor bits from `AudioControl.cs`: type `word & 0x3f`, volume `0x40`, mute `0x80`, source `0x100`, recording `0x200`, custom-name `0x400`, dB-format `0x800`. The upper nibble's channel semantics need checking; the decompiled constructor casts the masked value directly to a byte, so it is not used by this tool.

| Index | Descriptor | Type |
|---|---|---|
| 0 | `89c1` | speaker |
| 1 | `89c2` | headphone |
| 2 | `29ce` | S/PDIF output |
| 3 | `29cf` | microphone monitoring |
| 4 | `29d0` | line monitoring |
| 5 | `2ac3` | microphone input |
| 6 | `2ac4` | line input |
| 7 | `2ac5` | What U Hear recording |
| 8 | `29d1` | S/PDIF monitoring |
| 9 | `2ac9` | S/PDIF input |
| 10 | `00bf` | automatic gain control (mute-like boolean, no volume) |

Per-control queries:

```text
Level  5a 23 02 01 index
Mute   5a 24 02 01 index
Range  5a 22 21 01 index [31 zero bytes]
```

Range request reproduces the managed fixed-array struct: count `1` plus 32 index bytes. Range response: `count:u8 continuation:u8`, then tuples `index:u8 max:u16le min:u16le step:u16le`. Level replies begin `index:u8 value:u16le`; observed replies also carry one trailing zero byte. Mute response is `index:u8 value:u8`.

The dB-format controls appear to use signed Q8.8 values. This is **inferred**, based on the dB flag, negative values, and UAC conventions; it has not been calibrated against the UI or an acoustic measurement. Raw bytes are preserved alongside decoded estimates. A mute-like field for automatic gain control should not automatically be presented as ordinary audio mute.

Static setter layouts: `5a 23 04 00 index value:u16le` and `5a 24 03 00 index value:u8`. These mixer setters were not sent and are not exposed by the CLI. Use ALSA/PipeWire for normal volume handling until semantics are verified.

## 9. SXFI and hardware buttons — partial

```text
Capabilities TX  5a 6f 01 00
Capabilities RX 5a 6f 08 00 03 01 87 02 01 1c 01
Headphone info  5a 6f 01 06
Button state    5a 26 03 08 ff ff
State RX        5a 26 0b 08 ff ff 00 00 00 00 00 00 00 00
```

The headphone-info response carried 65 payload bytes, which is longer than the advertised **outbound command** maximum of 59. Do not apply that maximum indiscriminately to received frames.

The native managed button getter serializes only `operation + button_id:u16`; a mask member is used for client-side interpretation and is **not sent**. An earlier exploratory query in `capability-reads.json` included extra `ff` bytes; the canonical `08 ff ff` query was subsequently confirmed in `mixer-reads.jsonl`.

Static button setter: `5a 26 05 07 button_id:u16le state:u16le`. SXFI's advertised ID is 30 (`1e 00`). This identifies a candidate SXFI toggle path but it was **not tested**, and state-bit indexing must be checked against the capability ordering. SXFI personalization uploads and profile matching remain unresolved; no such data was written.

## 10. Linux integration and remaining work

The Linux controller should operate the CDC tty, leaving audio and HID interfaces with their class drivers. A desktop control service can expose the settings through D-Bus or a GUI. An ALSA external-control plugin could bridge these settings without inventing a new PCM driver.

Before claiming a full Linux driver/replacement:

1. Run `status`, `mixer`, and `profiles` on Linux, both immediately after power-on and after reconnect/suspend.
2. Verify Direct Mode, routing, high-gain, and EQ band changes with readback and physical/acoustic observation, restoring original settings.
3. Capture Creative App traffic for SXFI, speaker configuration, SmartComms, and any desired remaining feature. Determine which are device commands versus Windows APO processing.
4. Verify PCM playback/recording, clock ranges, channel mapping, and sample-rate transitions with the chosen Linux kernel.
5. Establish persistence and recovery behavior before implementing profile storage writes.

The static catalogue includes upgrade/reset and commands for unrelated models. It is evidence for targeted analysis, not a list to probe indiscriminately.
