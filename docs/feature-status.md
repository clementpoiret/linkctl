# Feature status

This document is the durable product boundary for `linkctl` 1.0. Runtime discovery remains authoritative: a feature
listed here is usable only when the selected device advertises the required standard interface or matches the exact
verified profile described in the [compatibility matrix](compatibility.md).

Status terms:

- **Supported:** implemented in the standard package and validated within the stated boundary.
- **Conditional:** implemented, but dependent on an exact profile, optional system component, or narrower tested path.
- **Experimental:** excluded from standard packages and carrying no release compatibility promise.
- **Unavailable:** no complete user-facing implementation is shipped.
- **Prohibited:** intentionally blocked by the safety policy.

## Supported in the standard package

| Area | Supported behavior |
|---|---|
| Discovery and diagnostics | Stable USB-based selection, grouped video/audio/maintenance nodes, hotplug JSONL, live capabilities, redacted probes, `doctor`, diagnostic bundles, completions, and manuals |
| Standard controls | Live V4L2 enumeration; generic get/set/reset/watch; semantic brightness, contrast, saturation, sharpness, gain, backlight compensation, white balance, focus, anti-flicker, and digital zoom |
| Video and images | Exact advertised tuple enumeration/negotiation, statistics, JPEG/PNG/raw snapshots, binary stdout capture, H.264/MJPEG pass-through, Matroska/MP4 recording, segmentation, rolling limits, and disk guards |
| Audio | PipeWire/ALSA discovery, camera association, hardware/host gain and mute, WAV/FLAC/raw capture, metering, monitoring, resampling, optional gate/compressor/limiter, and optional A/V recording |
| Presets | Strict schema-2 semantic camera/image/gesture/pickup state and raw V4L2 snapshots; immutable `builtin:default`; local user presets; selective capture, dry-run plans, live defaults, readback, reverse rollback, and recovery journals |
| Local service | Owner-only protocol-1 IPC, one selected camera per daemon, serialized controls, one shared source, snapshots, background recording, graph/metrics, and bounded hotplug recovery |
| Research reads | Runtime XU inventory, exact safe reads, repeated snapshots, volatility-aware diffs/watch, profile decoding, redacted evidence, and bounded handle/pipeline recovery |
| Firmware maintenance | Read-only version/status, normal-to-U-Disk watch, validation and no-clobber staging of an explicit official file, synchronization, post-copy hashing, private logs, and manual reconnect verification |

Standard mutations validate the complete request before writing, take a per-device lease, verify the result, and
attempt rollback where the operation is reversible. Unknown camera firmware retains standard controls and safe reads
but does not inherit vendor writes.

## Exact-profile camera features

The following conditional features are verified for the recorded Link 2C Pro landscape, low-resolution, and native
portrait descriptors on firmware `v0.2.9.8_build3`:

| Capability | Boundary |
|---|---|
| Auto Framing | Status, on/off, Smart Composition prerequisite, and Head/Half-body style; no tracking-zone API |
| Image pipeline | Automatic/manual scalar exposure, ISO 100–3200, shutter 1/8000–1/30, ±3.0 EV compensation, HDR, horizontal mirror, and vertical flip; write-only exposure curves unavailable |
| Audio pickup | Standard, Wide, Focus, and Original camera-native modes, independent of Linux gain/mute |
| Whiteboard and DeskView | Camera-resident Whiteboard; camera-native DeskView and 10–80 vertical correction; no host surface detection or perspective correction |
| Gestures | Global and individual Palm, V-sign, and L-sign switches for Auto Framing, Whiteboard, and Zoom |
| Compatibility | Restart-dependent standard/low-resolution switch; low-resolution advertises verified 640×360 MJPG, H.264, and YUYV tuples |
| Native portrait | Restart-dependent 736×1280, 1080×1920, 1088×1920, and 2176×3840 tuples; physical camera rotation required; mutually exclusive with low-resolution mode |
| Firmware | Read-only firmware string from the exact mapped selector |

White balance, focus, anti-flicker, ordinary image controls, and zoom use live standard V4L2 controls rather than the
vendor profile where advertised. Camera-native Auto Framing, Whiteboard, DeskView, and gesture status may briefly open
a no-output stream at the current tuple because those selectors return inactive values while streaming is closed.

## Conditional and affected behavior

| Area | Boundary |
|---|---|
| Multiple cameras | Discovery and explicit direct selection work; one camera is supervised per `linkd` instance and group transactions are unavailable |
| Frame translation | The host graph has crop/position primitives, but no verified camera-native translation mapping or user-facing host tracking workflow |
| Privacy | `privacy status` honestly separates unknown physical-shutter state, stream state, and hardware/host audio mute; logical privacy enter/exit is unavailable |
| Audio processing | Fixed opt-in gate/compressor/limiter and resampling are supported; EQ, AGC, echo cancellation, and advanced DSP are unavailable |
| Recording | Foreground/background recording, segmentation, rolling retention, and reconnect siblings are supported; pre-event recording and chapter markers are unavailable |
| Virtual outputs | Named raw branches, explicit caps, base transforms, counters, and multiple internal consumers are implemented; general desktop camera compatibility is not supported on the tested kernel-module combination |
| Firmware staging | Identity and U-Disk volume detection were hardware-validated; no firmware image was staged during project validation |

### v4l2loopback limitation

The tested v4l2loopback 0.15.4 module fails its streaming output queue when GStreamer's `v4l2sink` and streaming
consumers interact under `exclusive_caps=1`. Read/write sink mode does not activate the capture-only state required by
WebRTC discovery, while streaming I/O can fail allocation/queue handling and disturb the shared graph. The upstream
work is tracked in [v4l2loopback pull request 656](https://github.com/v4l2loopback/v4l2loopback/pull/656).

This limitation does not affect the physical camera, controls, IPC, snapshots, file recording, audio, internal raw
branches, or file/network sinks. Packages do not install or load v4l2loopback automatically. OBS and Chromium/WebRTC
virtual-camera use are therefore not release-supported.

## Experimental builds

- The non-default `network` feature adds typed RTP/UDP output. RTSP, SRT, integrated WebRTC, gateways, and arbitrary
  pipeline strings remain unavailable.
- The non-default `research` feature includes raw XU transport, but a write still requires configuration permission,
  `--unsafe-xu`, an exact experimental/verified profile, an advertised selector and length, an allowed safety class,
  the required stream state, a device lease, and rate limiting. It has no standard-package support promise.
- `--no-default-features` is a compile-only configuration without the native media backend, not a release package.

## Unavailable behavior

- Host Auto Framing, person detection/tracking, active-speaker framing, regions, smoothing, and host gesture recognition.
- Smart Whiteboard detection/calibration/rectification/freeze/OCR, host DeskView correction, document mode, and host
  portrait orchestration.
- Background segmentation, blur/replacement, chroma key, relighting, filters/LUTs, appearance processing, and bundled
  or automatically downloaded models.
- Multi-camera supervision in one daemon, coordinated group presets, D-Bus, HTTP, WebSocket, MQTT, MIDI, OSC,
  Prometheus export, remote pairing, cloud services, and remote event subscriptions.
- Logical privacy transitions, verified shutter-position sensing, touch-key control, and indicator control/readback.
- Timelapse, trigger engine, pre-event recording, chapter markers, and arbitrary restreaming protocols.

Unavailable commands are omitted or return `capability-unsupported`; placeholders are not presented as working
features.

## Prohibited operations

No build authorizes firmware/flash writes through device controls, forced bootloader entry, calibration writes,
mechanical pan/tilt/gimbal/motor writes, kernel-driver detach, or USB reset. Raw discovery may report kernel-advertised
pan/tilt fields, but semantic movement commands and options do not exist for this fixed-mount camera.

## Validation baseline

Physical-camera validation on x86-64 included:

- Three control cycles with readback/restoration for standard and verified vendor controls.
- Low-resolution and physically rotated native-portrait restart, tuple, snapshot, and restoration checks.
- 30 minutes of 4K30 H.264 pass-through: 53,928 frames, one sequence drop.
- 60 minutes of 1080p60 H.264 plus 48 kHz mono FLAC: 215,931 video frames, one sequence drop, no reported audio or
  video timestamp discontinuities, and 5.205 ms measured A/V drift.
- Daemon snapshots and recording recovery across unplug/replug while preserving the H.264 1920×1080 at 60 fps tuple;
  recording produced separately playable original and `.reconnect-001` files.
- One physical 1080p30 source feeding two differently transformed raw output branches, with approximately 37–39 ms
  recent p95 processing latency during the recorded test.

AArch64 packages receive the same locked build, parser/ABI tests, and package checks but have not received the same
physical-camera validation. Exact supported distributions and runtime floors are in [Compatibility](compatibility.md).
