# Link 2C Pro camera-native capabilities

`linkctl caps all` is the authoritative runtime view of the fixed-mount camera's semantic capabilities. Each item reports its state, backend, evidence, readable/writable flags, profile identity and checksum when applicable, live value when readable, persistence, and stream/restart dependency. The command also works in U-disk mode, where it reports camera controls as unavailable without trying to open a video node.

The following outcomes are established for the currently recorded landscape descriptor `1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c` and USB revision `0200`:

| Capability group | Command surface | Current outcome |
| --- | --- | --- |
| Digital zoom | `zoom get/set/step/ramp/reset` | Verified standard V4L2 `zoom_absolute`, 1.00x–4.00x in 0.01x steps; write/readback/restore verified on hardware. |
| Frame translation | `frame status/set/move/center` | Discovered, vendor transport unmapped. No pan/tilt substitution. |
| Auto Framing | `auto-framing on/off/status/style` | Status, on/off, and Head/Half-body selection are verified for firmware `v0.2.9.8_build3`. No tracking-zone commands are exposed. |
| Image pipeline | existing `image` commands plus `hdr`, `mirror`, and `flip` | HDR status and on/off are verified for firmware `v0.2.9.8_build3`; standard V4L2 controls are preferred when present. Other camera-native image items remain explicitly unmapped. |
| Pickup mode | `audio mode status/standard/wide/focus/original` | Discovered, transport unmapped. Host gain/mute/filter controls remain separate. |
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

## Mapping checkpoint

The remaining mappings require differential captures from the official Windows controller. Capture one setting at a time with a stable stream state, repeat each transition at least three times, and include a no-change control run. Use the private workflow in [Safe UVC Extension Unit research](xu-research.md). Do not issue an unknown `SET_CUR` from Linux; captured transfers are evidence to review, not an automatic write allow-list.

For every candidate mapping, record the payload bytes before and after, volatile-bit mask, selector GUID/length, firmware source/value, stream requirement, persistence across stream restart/reconnect, restoration transfer, controller version, and visual/readback observation. A mapping is complete only after its semantic command has a bounded restore path and target-hardware validation produces no camera disappearance or USB/UVC kernel error.
