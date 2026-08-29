# Link 2C Pro camera-native capabilities

`linkctl caps all` is the authoritative runtime view of the fixed-mount camera's semantic capabilities. Each item reports its state, backend, evidence, readable/writable flags, profile identity and checksum when applicable, live value when readable, persistence, and stream/restart dependency. The command also works in U-disk mode, where it reports camera controls as unavailable without trying to open a video node.

The following outcomes are established for the recorded Standard descriptor `1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c`, Low resolution descriptor `f8ec69e87774e9831bef86c498625373ba5fbb9d22bbc558185d88fb271a1bc2`, and USB revision `0200`:

| Capability group | Command surface | Current outcome |
| --- | --- | --- |
| Digital zoom | `zoom get/set/step/ramp/reset` | Verified standard V4L2 `zoom_absolute`, 1.00x–4.00x in 0.01x steps; write/readback/restore verified on hardware. |
| Frame translation | `frame status/set/move/center` | Discovered, vendor transport unmapped. No pan/tilt substitution. |
| Auto Framing | `auto-framing on/off/status/style` | Status, on/off, and Head/Half-body selection are verified for firmware `v0.2.9.8_build3`. No tracking-zone commands are exposed. |
| Image pipeline | existing `image` commands plus `hdr`, `mirror`, and `flip` | Exposure auto/manual mode, ISO, shutter, exposure compensation, HDR, horizontal mirror, and vertical flip are verified for firmware `v0.2.9.8_build3`. White balance, focus, and anti-flicker are verified through standard UVC controls. Other camera-native image items remain explicitly unmapped. |
| Pickup mode | `audio mode status/standard/wide/focus/original` | All four camera-native modes are verified for firmware `v0.2.9.8_build3`; host gain/mute/filter controls remain separate. |
| Regular Whiteboard Mode | `mode whiteboard on/off/status` | Camera-resident mode is verified for firmware `v0.2.9.8_build3`; Smart Whiteboard remains a separate host pipeline. |
| DeskView | `mode deskview on/off/status/vertical-correction` | Camera-native mode and vertical correction are verified for firmware `v0.2.9.8_build3`. Host calibration, perspective correction, and virtual-camera output remain separate. |
| Gestures | `gesture status/enable/disable/set` | Global and per-gesture controls are verified for firmware `v0.2.9.8_build3`. |
| Native portrait | `portrait status/native` | Discovered, including correction switches; re-enumeration mapping pending. |
| Compatibility | `mode compatibility status/set` | Standard and restart-dependent Low resolution/360p modes are verified for firmware `v0.2.9.8_build3`; the separate YUY2 switch remains unmapped. |
| Firmware | `firmware info` | Verified read-only landscape mapping: XU GUID `faf1672d-b71b-4793-8c91-7b1c9b7f95f8`, selector 3, 234-byte payload, UTF-8 field at offset 97. Observed version: `v0.2.9.8_build3`. |
| Hardware, mode, error, indicator | `firmware info`, `device state` | Read-only semantic sources unmapped. USB `bcdDevice` is not reported as firmware. |
| Physical privacy shutter | `privacy status` | Hardware-only; no verified position readback. Host and hardware audio mute remain independently reported. |

An unmapped mutation returns `capability-unsupported` before opening a writable XU session. It does not copy payloads from Link, Link 2, or Link 2 Pro research. A writable vendor item becomes available only through a compiled-in, reviewed profile matching VID/PID, USB revision, exact descriptor fingerprint, exact firmware, XU GUID, selector, and live `GET_LEN`. The firmware field is read-only and excluded from ordinary snapshots because the surrounding payload contains unrelated device-specific data.

## Auto Framing mapping

The verified mapping uses XU GUID `faf1672d-b71b-4793-8c91-7b1c9b7f95f8`, selector 2, and an exact 61-byte payload. In the reviewed controller capture, byte 0 was `0x07` for on and `0x00` for off, and periodic `GET_CUR` responses confirmed the state byte after every transition.

The controller committed MJPEG at 1920×1080 and 30 fps 12.45 seconds before its first successful mutation, and issued two `GET_LEN` requests immediately before each `SET_CUR`. That interval was an observation rather than a proven minimum. Linux timing sweeps subsequently verified four-second, two-second, and one-second warm-ups, followed by 500-millisecond delayed readback. The exact-firmware profile therefore uses a one-second temporary no-output stream warm-up, validates both length replies, submits the captured template, and verifies `GET_CUR` after 500 milliseconds. A failed readback still triggers semantic rollback.

Selector 2 returns the inactive value while the camera stream is closed, even when Auto Framing is configured on. Status therefore holds a short no-output stream at the camera's current video tuple while reading stream-dependent controls. A timing sweep observed the active value within one second at both MJPEG 1920×1080 at 30 fps and 1920×1440 at 60 fps, so the read path uses a bounded one-second warm-up. This remains separate from the exact-format write path and preserves the configured video tuple.

Smart Composition and framing style are separate controls on the same XU. Smart Composition occupies bit 0 of the two-byte little-endian selector 27: `d4 01` is off and `d5 01` is on when HDR remains enabled. Selector 19 is a one-byte enum: `01` is Head and `02` is Half-body. Each transition repeated in the controller capture and was confirmed by its following `GET_CUR`; Linux target-hardware tests then verified Smart Composition `off → on → off` and style `Head → Half-body → Head`.

`auto-framing style head` and `auto-framing style half-body` run as a two-step semantic transaction under one device lease and one warmed stream. The command first enables Smart Composition, then sets the requested style. Both stages use 500-millisecond delayed readback, and failure restores every attempted control in reverse order. Three complete target-hardware cycles verified Auto Framing `off → on` and style `Half-body → Head → Half-body` with the selected timing. The command configures the camera-native style but does not implicitly turn Auto Framing on; use `auto-framing on` separately when framing should become active.

## HDR mapping

HDR shares selector 27 with Smart Composition and occupies bit 2. The reviewed controller capture began at `d5 01` with both settings enabled, then repeated `d1 01` for HDR off and `d5 01` for HDR on three times. Periodic `GET_CUR` responses confirmed each resulting value. Every mutation was preceded by a current-value read and two `GET_LEN` requests, and the video stream remained open throughout.

The two profile controls therefore use masked read-modify-write encoding. `image hdr` changes only bit 2, and Auto Framing style's Smart Composition prerequisite changes only bit 0; both preserve the rest of the freshly read two-byte value. The HDR path uses the trace-matched MJPEG 1920×1080 at 30 fps stream, the hardware-validated one-second warm-up and 500-millisecond readback delay, and semantic rollback on a mismatch. Automatic backend selection still prefers a standard V4L2 HDR control if one is advertised.

## Flip mappings

The reviewed `flips.pcap` Controller capture (SHA-256 `2d36337eb00d67a7aa5bb838d26f2301f1abebbfca50a2d1e85ffe38fed450db`) maps both camera-native flip settings onto the same selector 27 value. Starting from the default-off `d5 01` payload, horizontal mirror set bit 3 of the little-endian word (`dd 01`) and vertical flip set bit 12 (`d5 11`). Repeated writes returned to `d5 01`, and later `GET_CUR` samples confirmed every on and off state.

`image mirror` and `image flip` use positive masks `0x0008` and `0x1000` respectively, merge into a freshly read baseline, preserve the other selector fields, and require exact delayed readback. They use the same trace-matched stream and timing as the already validated selector-27 controls. Linux hardware validation completed three on/off cycles for each direction, then verified the combined `dd01 → dd11 → d511 → d501` sequence. HDR stayed on throughout, and both flip controls were restored off. No standard V4L2 flip controls were advertised on the target camera.

## Exposure mapping

The reviewed Controller capture maps exposure controls on XU GUID `faf1672d-b71b-4793-8c91-7b1c9b7f95f8`. Selector 30 is a one-byte mode enum (`01` manual, `02` auto), selector 25 is a two-byte little-endian ISO value, and selector 29 is a two-byte little-endian shutter denominator. The trace exercised ISO 100, 320, and 3200 and shutter denominators 30, 100, 200, and 8000. Each scalar has direct `GET_CUR` readback, and writes use the Controller's double-`GET_LEN` prelude while video is open. Hardware tests found one-unit shutter quantization at fractional-rate values (30→29, 60→59, 120→119, and 240→239), while 31, 100, 125, 200, and 8000 read back exactly. The shutter profile therefore permits a numeric readback difference of at most one; every other mapped control still requires exact equality.

Exposure compensation uses selector 9 as a two-byte signed little-endian value in hundredths of an EV. The Controller capture repeated `-300`, `0`, and `+300` three times for its -3.0, 0.0, and +3.0 EV positions. `image exposure-compensation` accepts that range in the UI's 0.1 EV steps, while status converts the direct raw readback back to EV. Automatic backend selection prefers a standard V4L2 exposure-compensation control when one is advertised.

`image exposure manual` writes mode first and then only the supplied ISO or shutter fields. A failure restores attempted controls in reverse order. `image exposure auto` changes only the mode. Automatic backend selection continues to prefer standard V4L2 exposure controls when they exist. The combined status value reports `mode`, `iso`, and a fractional `shutter` string.

Selector 16 carries the Controller's exposure curve as three 255-byte writes per update. The capture contains 369 writes and double `GET_LEN` requests but no `GET_CUR`, leaving no bounded readback or rollback path. Curve mutation therefore remains intentionally unavailable and is not part of the verified scalar exposure mapping.

## White balance mapping

The reviewed controller capture confirms that white balance bypasses the vendor profile. It uses UVC Processing Unit entity 5: selector 11 is the one-byte automatic-mode control (`00` manual, `01` automatic), and selector 10 is the two-byte little-endian temperature control. The controller repeated every mode transition and each 2000 K, 4800 K, and 10000 K selection; manual selections disabled automatic mode before writing the temperature, and the session ended at 4800 K with automatic mode enabled.

Linux exposes these as `white_balance_automatic` and `white_balance_temperature`. `linkctl` uses their live V4L2 descriptors for access, range validation, readback, and rollback. The target descriptor reports 2000–10000 K in 1 K steps. Manual temperature transactions disable automatic mode first and re-query the temperature descriptor after that prerequisite changes state. Hardware readback verified both endpoints, 4800 K, and three complete manual/automatic cycles; the final transaction restored 4800 K before enabling automatic mode. The measured semantic mutations completed in 0.21–0.36 seconds. No firmware-specific XU mapping is involved.

## Focus mapping

The reviewed `auto-focus.pcap` Controller capture (SHA-256 `da1bf61472b3ed002b4dcf3497b1c07dff52885ac404178453fd41afb0a1b670`) confirms that focus also bypasses the vendor profile. It uses UVC Camera Terminal entity 1: selector 8 is the one-byte autofocus control (`00` manual, `01` automatic), and selector 6 is the two-byte little-endian absolute-focus control. The Controller repeated three automatic/manual cycles, exercised the absolute endpoints, and finished with autofocus enabled. Manual transitions disabled autofocus before writing the absolute value.

Linux exposes these controls as `focus_automatic_continuous` and `focus_absolute`. The target descriptor reports raw absolute focus from 0 through 100 in steps of one. `image focus manual` presents that as a reversible normalized 0.0–1.0 position, disables autofocus first, re-queries the formerly inactive absolute control, and verifies both writes. `image status` reports the live mode and normalized position together while retaining the raw descriptor and current value in JSON output. Target-hardware validation completed three `0.0 → 1.0 → auto` cycles, verified the intermediate `0.37` as raw `37`, and rejected out-of-range and non-finite values before writing. Mutations completed in 0.06–0.36 seconds, and the original raw `100` plus autofocus-enabled state were restored. No firmware-specific XU mapping is involved.

## Anti-flicker mapping

The reviewed `anti-flicker.pcap` Controller capture (SHA-256 `a8c66ac6086ece24866dadeee506a07a0c50a48458688cba27610392c077e2ef`) confirms that anti-flicker uses UVC Processing Unit entity 5, selector 5, as a one-byte enum. The repeated Controller transitions identify raw `1` as 50 Hz, raw `2` as 60 Hz, and raw `3` as automatic. The session ended at 50 Hz.

Linux exposes the control as `power_line_frequency`. On the target system its live descriptor advertises disabled (`0`), 50 Hz (`1`), and 60 Hz (`2`), but reports the unadvertised automatic value (`3`) as its default. The V4L2 control layer rejects `3` as out of range, so `linkctl` reports only the three writable live menu choices and rejects `auto` with `capability-unsupported` before opening the control for writing. No raw USB transfer or driver detach is used to bypass that kernel contract.

Target-hardware validation verified writes and direct readback for disabled, 50 Hz, and 60 Hz, followed by restoration to 50 Hz. Mutations completed in 0.22–0.33 seconds. `image status` renders the current enum semantically, while `caps controls` retains the complete live descriptor, including its invalid default, for diagnosis.

## Audio pickup-mode mapping

The reviewed `audio-modes.pcap` Controller capture (SHA-256 `71f9a37a70a159ab07e9eba3f42bfc7e920ce649889b9f4832c365ee4a6fb74a`) maps pickup mode to XU GUID `e307e649-4618-a3ff-82fc-2d8b5f216773`, selector 31, as a one-byte enum: Standard `00`, Wide `01`, Focus `02`, and Original `03`. The trace repeated the ordered mode changes and its periodic `GET_CUR` reads confirmed each written value; it finished at Focus.

The verified profile checks the one-byte length twice immediately before each write, requires exact delayed readback, rate-limits mutations, and restores the previous enum after a mismatch. The Controller trace covered writes with preview running, while Linux hardware verification completed three full mode cycles with video closed, so no video stream is required. The measured mutations completed in 0.52–0.66 seconds and Focus was restored at the end. The profile conservatively leaves reconnect and power-cycle persistence undeclared because the observed final value is also the stated default.

## Camera-native DeskView mapping

The reviewed `deskview.pcap` Controller capture (SHA-256 `d134801e2564ac699bc3c59629faa55cc812ab564d96c61d8d0ae38b2c031b78`) maps DeskView to mode value `06` and off to `00` in the first byte of XU GUID `faf1672d-b71b-4793-8c91-7b1c9b7f95f8`, selector 2, length 61. Periodic `GET_CUR` samples confirmed each transition. This is the camera-resident transform selected by `mode deskview`; it does not provide the named calibration, arbitrary corner correction, or virtual-camera publication of the separate host DeskView workflow.

Vertical correction is a signed little-endian value at bytes 52–53 of the same selector. The wire value is negative ten times the whole-number setting: 10 is `-100` (`9c ff`), the default 45 is `-450` (`3e fe`), and 80 is `-800` (`e0 fc`). Each value was written repeatedly and confirmed by later reads. The Controller uses a stable 61-byte write template rather than echoing the selector's volatile status bytes, so the verified profile reproduces that template and changes only the correction field. Setting correction also selects DeskView; values outside 10–80 are rejected before device access.

Linux hardware validation completed three `on → 10 → 80 → 45 → off` cycles with exact semantic readback and an additional idempotent off restoration. Mode commands completed in 4.81–5.33 seconds and correction commands in 2.81–2.84 seconds, including temporary stream startup and verification. The camera was restored to DeskView off with correction 45. Reconnect and power-cycle persistence remain unverified.

## Camera-native Whiteboard mapping

The reviewed `whiteboard.pcap` Controller capture (SHA-256 `413fd7c986b99dfe0789e7bed6a10639e6236eba8c0d3dff4a77249bb5a43d20`) maps regular camera-resident Whiteboard to mode byte `04` and off to `00` on XU GUID `faf1672d-b71b-4793-8c91-7b1c9b7f95f8`, selector 2, length 61. The Controller wrote regular Whiteboard on three times and off twice before the Smart Whiteboard attempt; periodic `GET_CUR` samples confirmed each transition after approximately one second. No gesture-selector write accompanied these changes, so activating regular Whiteboard does not rewrite the configured V-sign gesture switch.

The later Smart Whiteboard sequence is deliberately not mapped as a camera-native mode. It used mode byte `0a`, additional controls on a second Extension Unit, and stream negotiation associated with the Insta360 virtual camera. That is consistent with Smart Whiteboard being a host/virtual-camera workflow rather than the regular camera-resident transform. `linkctl mode whiteboard` therefore exposes only the repeated `04 ↔ 00` mapping; the independent `smart-whiteboard` command family remains the host-side interface.

The verified profile starts the trace-matched MJPEG 1920×1080 at 30 fps stream, waits one second, checks the 61-byte selector length twice, writes a bounded mode template, and performs exact readback after the selector's bounded transition interval with rollback on mismatch.

Linux hardware validation first observed the selector's transient `ff` mode during an off transition at a 1.25-second readback; the command rejected the mismatch and successfully restored Whiteboard on. Extending readback to 2.25 seconds then passed three complete on/off cycles. Whiteboard was restored off, while Palm, V-sign, and L-sign gesture settings all remained on. Reconnect and power-cycle persistence remain unverified.

## Camera-native gesture mapping

The reviewed `gestures.pcap` Controller capture (SHA-256 `92c80aded28ca497db18727bae5ba590dddf93ceb9ffbc3c0bb8fc92dcde059c`) maps the gesture switches to a one-byte mask on XU GUID `faf1672d-b71b-4793-8c91-7b1c9b7f95f8`, selector 5. Palm/Auto Framing uses `0x02`, L-sign/Zoom uses `0x04`, and V-sign/Whiteboard uses `0x08`; all three on is `0x0e`. Repeated writes and periodic `GET_CUR` reads confirmed the independent transitions `0x0e ↔ 0x0c`, `0x0e ↔ 0x06`, and `0x0e ↔ 0x0a`, and the capture ended with all three switches on.

Each per-gesture command uses a masked read-modify-write, preserving both the other gesture switches and the currently unassigned `0x01` bit. `gesture enable` sets all three verified bits and `gesture disable` clears them in one masked write; a partial configuration is reported as its enabled gesture combination. The verified profile follows the captured running-preview transfer sequence with two length queries before each write and delayed exact readback.

Linux hardware validation verified the aggregate all-off/all-on transition and three complete off/on cycles for each individual switch. Exact status readback showed the two untargeted switches remained on throughout every individual off state. The camera was restored to Palm, V-sign, and L-sign all on. Reconnect and power-cycle persistence remain unverified.

## Low-resolution compatibility mapping

The reviewed `low-res.pcap` Controller capture (SHA-256 `1932784a09198ee5d8c5967f1ee6ae5c17306d24c970dc18e7c75ce2bac6a6ab`) maps Low resolution to bit `0x2000` of the two-byte little-endian selector 27 value. With the other captured settings preserved, Standard read as `d5 01` and Low resolution as `d5 21`. Two complete enable/disable cycles repeated those values and showed the camera begin disconnecting about 180 milliseconds after every changed write; there is no separate restart command.

The restart changes the full USB descriptor fingerprint. Standard mode uses `1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c`. Low resolution uses `f8ec69e87774e9831bef86c498625373ba5fbb9d22bbc558185d88fb271a1bc2` and adds 640×360 entries, including the captured 24, 25, and 30 fps intervals. Post-restart selector reads returned `d5 21` after enable and `d5 01` after disable.

`mode compatibility set low-resolution` and `set standard` perform a masked read-modify-write so HDR, flips, and the other selector 27 settings are preserved. A changed value waits up to the larger of the configured timeout and 15 seconds for an observed removal and changed-descriptor re-enumeration, reopens the new video node, requires the exact descriptor and firmware profile, and verifies the requested selector state. Requesting the already active value is a verified no-op and does not restart the camera. Automatic rollback is unavailable once the camera has disconnected; the bounded restore operation is an explicit `set standard`, which follows the same verified restart path. The separate YUY2 choice is rejected before a writable XU session is opened because this trace does not map it.

VirtualBox USB filters can automatically recapture the camera when it returns with the changed descriptors. Shut down the VM or disable its automatic camera filter before changing compatibility mode; otherwise the setting can apply while Linux receives no video node with which to verify or restore it. In that case `linkctl` reports that the re-enumeration occurred but the capture/control node remained unavailable.

Linux hardware validation with the VM shut down completed Standard-to-Low resolution in 7.79 seconds and restored Standard in 7.76 seconds. Both commands matched the expected post-restart descriptor and selector state. Low resolution advertised MJPEG, H.264, and YUYV 640×360 at 24, 25, and 30 fps, and a raw MJPEG snapshot produced a valid 640×360 frame. An idempotent Low resolution request completed in 0.34 seconds without changing the USB device number. The final Standard restoration returned to the original descriptor and removed every 640×360/640×480 compatibility-only format.

## Mapping checkpoint

The remaining mappings require differential captures from the official Windows controller. Capture one setting at a time with a stable stream state, repeat each transition at least three times, and include a no-change control run. Use the private workflow in [Safe UVC Extension Unit research](xu-research.md). Do not issue an unknown `SET_CUR` from Linux; captured transfers are evidence to review, not an automatic write allow-list.

For every candidate mapping, record the payload bytes before and after, volatile-bit mask, selector GUID/length, firmware source/value, stream requirement, persistence across stream restart/reconnect, restoration transfer, controller version, and visual/readback observation. A mapping is complete only after its semantic command has a bounded restore path and target-hardware validation produces no camera disappearance or USB/UVC kernel error.
