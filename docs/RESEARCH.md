> Research snapshot from the sibling `research/` bundle. References to the Python CLI and evidence files describe that bundle; see [VALIDATION.md](VALIDATION.md) for this Rust fork.

# Evidence, sources, and reproduction

## Sources used

1. [Creative's X4 support page](https://support.creative.com/products/productdetails.aspx?prodid=23705), identifying the current Creative App download.
2. [Creative-hosted installer](https://files.creative.com/creative/bin/apps/swureleases/win/creativeapp/release/CreativeAppSetup_1.24.05.01.exe). The installer and both analyzed DLLs passed Windows Authenticode verification with Creative Technology Ltd as signer. `evidence/signatures.json` records status value 0 (`Valid`).
3. [LukasWestholt's X3 protocol research](https://github.com/LukasWestholt/usb-reverse-engineering-sound-blaster-x3), especially the serial channel, challenge-response algorithm, and output-routing packets. Tree inspected: `b8c90fdaa0b2cc1b2a884482b6c0dd9ab373dff8`. Its X4 USB-ID claim and profile-page terminology were not copied as fact; local evidence corrected both.
4. [SoundBlaster-X4-ARM64](https://github.com/npark2860-cyber/SoundBlaster-X4-ARM64), especially [native CTCDC session tracing](https://github.com/npark2860-cyber/SoundBlaster-X4-ARM64/blob/main/DEBUG_HISTORY_20260903_CTCDC_NATIVE_UNLOCK_TRACE.md) and [Direct Mode proof](https://github.com/npark2860-cyber/SoundBlaster-X4-ARM64/blob/main/DEBUG_HISTORY_20260903_DIRECT_MODE_RUNTIME_SUCCESS.md). Tree inspected: `d95d9fbeaa2b807c0b99ef6df88af2f5857537dd`. Its CTCDC binary hash matches the downloaded binary here.

Other Sound Blaster Linux repositories were searched, but the two above supplied the relevant CDC protocol precedents. The controller was written independently in this workspace; it does not load vendor binaries or import either project's implementation.

## Binary identities

| File | Bytes | SHA-256 |
|---|---:|---|
| `CreativeAppSetup_1.24.05.01.exe` | 91,161,688 | `0754362c9d6d55310fcebfbf161bb33b7b897957c2d29d1f2216d7365eb97918` |
| `CTCDC.dll` | 2,122,200 | `bc4010e8f7000bfe6217425a0622dd710a7626d90fb61008505337aa87a43dab` |
| `Creative.Platform.Devices.dll` | 2,431,216 | `2d77172fb6ae850b6d03a09830892c8c3a0ab79e10dda28f40a76b3fadc47e93` |

Machine-readable source: `evidence/provenance.json`.

## Extraction and decompilation

The installer was downloaded, signature-checked, and extracted with [innoextract 1.9](https://github.com/dscharrer/innoextract/releases/tag/1.9). It was not installed. The Windows machine remains bound to its original Microsoft USB audio and CDC drivers.

Local analysis tools were downloaded into `tools/`: Python 3.13.7 embedded, .NET SDK 10.0.403, and ILSpy command line 11.2.0.10050. `pyserial` and `pycryptodome` are the runtime dependencies; `pefile` was used for key-address/provenance inspection. Other downloaded Python analysis packages are not required by the controller. The .NET SDK's first run also reported creating its usual local ASP.NET development certificate; no trust command was run.

Reproduce from this workspace:

```powershell
.\tools\innoextract\innoextract.exe -e -d downloads\creative-app downloads\CreativeAppSetup_1.24.05.01.exe
$env:DOTNET_ROOT = Join-Path $PWD 'tools\dotnet'
$env:PATH = $env:DOTNET_ROOT + ';' + $env:PATH
.\tools\ilspy\ilspycmd.cmd -p -o evidence\decompiled\devices downloads\creative-app\app\Creative.Platform.Devices.dll
.\tools\python\python.exe tools\extract_protocol_metadata.py
```

ILSpy recovered the protocol classes despite control-flow/string obfuscation. It reported five unrelated method-decompilation errors: two task-runner methods, `USBDeviceStateService` construction, `ApoCDCStereoDirectFeature.SetEnabledAsync`, and a HID feature-checker method. Therefore this is not an assertion that every managed method was successfully decompiled. The low-level structs, command enum, key repository, and parameter mappings used here were recovered.

Important local decompilation paths under `evidence/decompiled/devices/`:

| Source | Useful content |
|---|---|
| `Creative.Platform.Devices.Models.CDCDevices/CDCRawLibrary.cs` | framing, enums, command structs, ACK layout, profile block assembly |
| `Creative.Platform.Devices.Models.CDCDevices/CDCConnection.cs` | raw get/set path and connection handling |
| `Creative.Platform.Devices.Features.CDCDevices/CDCFeatureChecker.cs` | capability query construction |
| `Creative.Platform.Devices.Features/AudioControl.cs` | mixer descriptor bit masks |
| `Creative.Platform.Devices.Models/VendorConstants.cs` | X4 PID = 12920 (`0x3278`) |
| `Creative.Platform.Devices.Models/ProductCodeNames.cs` | Accent2 = SB1815 |
| Root `gCcHvY...cs` file | repository mapping of getters and setters to numeric controls |

The numeric enum catalogue records source-line locations for 85 enums. For example, `CDCRawCommand` starts at line 86, `MalcolmPlaybackManagerCommand` at 190, the feature get struct at 924, raw effect set/get around 3911/4050, profile response at 1775, and feature flag definitions around 7515. These line numbers refer to this ILSpy output, not vendor source code.

## Live evidence index

| File | What it proves |
|---|---|
| `pnp-devices.json`, `drivers.json` | connected model, interfaces, and current Windows driver bindings |
| `usb-descriptors.json`, `usb-configuration.bin`, `usb-interfaces.json` | live descriptors and endpoints read through hub IOCTLs |
| `initial-identity-probe.json` | first real X4 challenge, at 9600 baud |
| `powershell-session-probe.json` | successful generated AES-GCM unlock, SW_MODE1 ACK, readiness, firmware, buttons |
| `session-probe.json` | successful warm Python session queries; this file supersedes the early unsuccessful Python probe output |
| `control-reads.json` | feature/output support, DSP support, responsive EQ and unresponsive legacy effects |
| `eq-write-read-restore.json` | preamp -9 to -9.5 to -9, with ACKs and matching readbacks |
| `capability-reads.json` | general info, SXFI read responses, and two genre profiles; historical labels say `page0/page2` |
| `mixer-reads.jsonl`, `mixer-decoded.json` | all advertised mixer controls queried individually, including raw ranges/levels/mutes |
| `cli-status.jsonl`, `status.json` | final independent CLI status read |
| `cli-profiles.jsonl`, `cli-profiles.json` | CLI reassembly of separate speaker and headphone profiles |
| `cli-mixer.jsonl`, `cli-mixer.json` | CLI mixer reads with explicitly inferred dB conversion |
| `cli-eq-set.jsonl` | final CLI ACK + readback when setting preamp to its original -9 dB |

These are **serial application-protocol captures**, not USBPcap bus traces. USB endpoint identities come from independent descriptor reads. USB packet boundaries, URB timing, and control-line setup packets are not present in the serial logs. No USB capture kernel driver was installed and no administrator elevation was used.

All recorded reads/writes performed by this task are local to the connected X4. The only audio-setting transition tested was the temporary 0.5 dB preamp reduction, restored and re-read. Unlock/software mode changes establish the control session and may persist while the device remains powered.

## Validation

Run:

```sh
python -m unittest discover -s tests -v
```

Twelve tests passed. They cover all two-part fragment boundaries of mixed 5A/5B captured frames, coalesced frames, partial/oversized input, a real successful challenge-response vector, embedded CRLF in a nonce, the cold-start state machine, failing ACKs, unrelated notifications, timeout handling, setter validation, captured EQ values, and profile block reassembly/order errors.

Hardware checks passed for warm session initialization, status, mixer reads, both profiles, and EQ setter ACK/readback. No Linux machine was connected for running these checks. No PCM playback/recording tests or firmware modifications were performed.

## Continuing the USB capture work

If additional features require Creative App comparison, Windows USBPcap plus Wireshark can capture the real bus traffic after installing their capture driver with the necessary administrator access. Select only the hub/device containing `041e:3278`. The descriptor endpoint pair is bulk `0x03` OUT and `0x82` IN; Wireshark display filtering can narrow to those endpoints and the device address. A commonly useful filter is:

```text
usb.transfer_type == 3 && (usb.endpoint_address == 0x03 || usb.endpoint_address == 0x82)
```

Field names for payload export vary with the dissector; inspect `usb.capdata` and CDC data fields such as `usbcom.data.out_payload`. Add a device-address filter when other devices share the bus. These filter instructions are for a future capture; no `.pcapng` is claimed in this result.

On Linux, `usbmon` can observe the CDC endpoints while the ordinary class drivers remain bound. Start with a status read and one isolated setting change; save the original value, capture the setter/ACK/query sequence, restore, and verify. Physical knob/button notifications can already be collected with the CLI's `monitor` command.

Priority unknowns: genuine Linux cold plug/reconnect, routing/high-gain/Direct Mode transitions on this unit, EQ center frequencies/limits/persistence, SXFI state-bit ordering and personalization transfer, SmartComms/device-vs-host processing, speaker/channel setup, and UAC2 rate/channel streaming validation.
