# Link 2C Pro camera-native capabilities

`linkctl caps all` is the authoritative runtime view of the fixed-mount camera's semantic capabilities. Each item reports its state, backend, evidence, readable/writable flags, profile identity and checksum when applicable, live value when readable, persistence, and stream/restart dependency. The command also works in U-disk mode, where it reports camera controls as unavailable without trying to open a video node.

The following outcomes are established for the currently recorded landscape descriptor `1d0fa40a5787adc39223e26a5262f3d5e1ba0421e17442487157905cbd2a066c` and USB revision `0200`:

| Capability group | Command surface | Current outcome |
| --- | --- | --- |
| Digital zoom | `zoom get/set/step/ramp/reset` | Verified standard V4L2 `zoom_absolute`, 1.00x–4.00x in 0.01x steps; write/readback/restore verified on hardware. |
| Frame translation | `frame status/set/move/center` | Discovered, vendor transport unmapped. No pan/tilt substitution. |
| Auto Framing | `auto-framing on/off/status/style` | Status, on/off, and Head/Half-body selection are verified for firmware `v0.2.9.8_build3`. No tracking-zone commands are exposed. |
| Image pipeline | existing `image` commands plus `hdr`, `mirror`, and `flip` | Standard V4L2 controls are used when present; otherwise the camera-native item is explicitly unmapped. |
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

The controller committed MJPEG at 1920×1080 and 30 fps 12.45 seconds before its first successful mutation, and issued two `GET_LEN` requests immediately before each `SET_CUR`. The exact-firmware profile reproduces those conditions with a 13-second temporary no-output stream warm-up, validates both length replies, submits the captured on/off template, and verifies `GET_CUR` after 1.25 seconds. Target-hardware tests verified both `off` to `on` and `on` to `off`; a failed readback still triggers a semantic rollback.

Smart Composition and framing style are separate controls on the same XU. Selector 27 is a two-byte little-endian enum: `d4 01` is off and `d5 01` is on. Selector 19 is a one-byte enum: `01` is Head and `02` is Half-body. Each transition repeated in the controller capture and was confirmed by its following `GET_CUR`; Linux target-hardware tests then verified Smart Composition `off → on → off` and style `Head → Half-body → Head`.

`auto-framing style head` and `auto-framing style half-body` run as a two-step semantic transaction under one device lease and one warmed stream. The command first enables Smart Composition, then sets the requested style. Both stages use delayed readback, and failure restores every attempted control in reverse order. The command configures the camera-native style but does not implicitly turn Auto Framing on; use `auto-framing on` separately when framing should become active.

## Mapping checkpoint

The remaining mappings require differential captures from the official Windows controller. Capture one setting at a time with a stable stream state, repeat each transition at least three times, and include a no-change control run. Use the private workflow in [Safe UVC Extension Unit research](xu-research.md). Do not issue an unknown `SET_CUR` from Linux; captured transfers are evidence to review, not an automatic write allow-list.

For every candidate mapping, record the payload bytes before and after, volatile-bit mask, selector GUID/length, firmware source/value, stream requirement, persistence across stream restart/reconnect, restoration transfer, controller version, and visual/readback observation. A mapping is complete only after its semantic command has a bounded restore path and target-hardware validation produces no camera disappearance or USB/UVC kernel error.
