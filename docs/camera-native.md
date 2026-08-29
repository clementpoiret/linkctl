# Link 2C Pro camera-native capabilities

`linkctl caps all` is the authoritative runtime view of the fixed-mount camera's semantic capabilities. Each item reports its state, backend, evidence, readable/writable flags, profile identity and checksum when applicable, live value when readable, persistence, and stream/restart dependency. The command also works in U-disk mode, where it reports camera controls as unavailable without trying to open a video node.

The following outcomes are established for the currently recorded landscape descriptor `1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c` and USB revision `0200`:

| Capability group | Command surface | Current outcome |
| --- | --- | --- |
| Digital zoom | `zoom get/set/step/ramp/reset` | Verified standard V4L2 `zoom_absolute`, 1.00x–4.00x in 0.01x steps; write/readback/restore verified on hardware. |
| Frame translation | `frame status/set/move/center` | Discovered, vendor transport unmapped. No pan/tilt substitution. |
| Auto Framing | `auto-framing on/off/status/style` | Status, on/off, and Head/Half-body selection are verified for firmware `v0.2.9.8_build3`. No tracking-zone commands are exposed. |
| Image pipeline | existing `image` commands plus `hdr`, `mirror`, and `flip` | Exposure auto/manual mode, ISO, shutter, exposure compensation, HDR, horizontal mirror, and vertical flip are verified for firmware `v0.2.9.8_build3`. White balance, focus, and anti-flicker are verified through standard UVC controls. Other camera-native image items remain explicitly unmapped. |
| Pickup mode | `audio mode status/standard/wide/focus/original` | All four camera-native modes are verified for firmware `v0.2.9.8_build3`; host gain/mute/filter controls remain separate. |
| Regular Whiteboard Mode | `mode whiteboard on/off/status` | Discovered, vendor transport unmapped. |
| Gestures | `gesture status/enable/disable/set` | Discovered, global/per-gesture mappings pending. |
| Native portrait | `portrait status/native` | Discovered, including correction switches; re-enumeration mapping pending. |
| Compatibility | `mode compatibility status/set` | Discovered, low-resolution and YUY2 switches unmapped. |
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

## Mapping checkpoint

The remaining mappings require differential captures from the official Windows controller. Capture one setting at a time with a stable stream state, repeat each transition at least three times, and include a no-change control run. Use the private workflow in [Safe UVC Extension Unit research](xu-research.md). Do not issue an unknown `SET_CUR` from Linux; captured transfers are evidence to review, not an automatic write allow-list.

For every candidate mapping, record the payload bytes before and after, volatile-bit mask, selector GUID/length, firmware source/value, stream requirement, persistence across stream restart/reconnect, restoration transfer, controller version, and visual/readback observation. A mapping is complete only after its semantic command has a bounded restore path and target-hardware validation produces no camera disappearance or USB/UVC kernel error.
