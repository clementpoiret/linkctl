# Current release state and deferred host features

This page is the maintainer handoff for the first `linkctl` release. It records what exists today, what has been
validated, what is deliberately not included, and where future development should resume. It is intentionally
self-contained so that the release boundary does not depend on historical planning material.

The status below was last validated on 2026-08-29 with an Insta360 Link 2C Pro running firmware
`v0.2.9.8_build3`, Linux `7.2.1-cachyos-lto`, GStreamer 1.28, and v4l2loopback 0.15.4. Hardware-dependent claims
apply to that tested combination unless a broader compatibility matrix says otherwise.

## Release boundary

The first release is a Linux camera-control, capture, and automation-ready CLI with a local single-camera stream
daemon. It includes verified camera-native controls, standard video and audio support, direct and daemon-owned media
operations, configuration and transactional presets, safe research tools, diagnostics, and the foundations of a
virtual-camera graph.

It does not include computer-vision host modes, advanced live effects, multi-camera daemon supervision, or remote
control services. Virtual-camera production is implemented, but OBS and WebRTC compatibility is not supported on the
tested host because the installed v4l2loopback module does not provide reliable streaming buffer queues.

## Current implementation

### Workspace components

| Component | Current responsibility and status |
|---|---|
| `link-core` | Shared configuration, errors and exit codes, output envelopes, device/media/control types, safety policy, presets, transaction planning, rollback journals, and application paths. Implemented and hardware-independent. |
| `link-linux` | USB identity, sysfs and udev discovery, stable selectors, node association, hot-plug observation, and U-Disk volume discovery. Implemented. |
| `link-v4l2` | V4L2 inventory, exact format negotiation, capture-node status, standard and extended controls, semantic value handling, dependency ordering, batching, readback, and rollback support. Implemented. |
| `link-audio` | ALSA and optional PipeWire discovery, camera association, gain and mute control, capture, metering, monitoring, resampling, basic processing, and A/V synchronization support. Implemented. |
| `link-profiles` | Strict profile loading and matching, trust classification, typed vendor codecs, exact descriptor and firmware guards, stream requirements, tail policies, and built-in verified Link 2C Pro mappings. Implemented. |
| `link-uvc-xu` | UVC descriptor parsing, exact Extension Unit reads, bounded verified writes, rate limiting, safe snapshots and diffs, and research-only raw access gates. Implemented. |
| `link-testkit` | Redacted recorded probe fixtures and hardware-free profile/inventory validation. Implemented. |
| `link-media` | Typed GStreamer pipelines for capture, snapshots, recording, audio muxing, pipes, optional RTP output, and the daemon's shared source graph. Implemented. |
| `link-ipc` | Versioned, length-bounded JSON and binary framing over a Unix socket with same-user peer authentication. Protocol version 1 is implemented. |
| `link-daemon` | One selected physical source, a serialized control actor, bounded graph requests, source ownership, recovery, snapshots, background recording, and named virtual-output branches. Implemented for one camera per daemon. |
| `link-cli` | User-facing discovery, controls, native modes, media, audio, presets, diagnostics, daemon, pipeline, and virtual-camera commands with human, JSON, and JSON Lines output. Implemented for the exposed command set. |
| `link-effects` | Reserved host transformation and computer-vision boundary. The crate and feature gate exist, but no host vision or AI implementation is present. |
| `link-sdk-bridge` | Reserved isolated vendor SDK boundary. The crate and feature gate exist, but no Link-compatible SDK integration is present. |

### Camera and control support

The current verified camera profile targets the recorded landscape, low-resolution, and native-portrait descriptors for
firmware `v0.2.9.8_build3`. It provides guarded access to camera-native Auto Framing and its Head/Half-body styles,
HDR, mirror and flip, scalar exposure settings, four microphone pickup modes, regular Whiteboard, camera-native
DeskView and vertical correction, three gesture switches, restart-dependent 360p compatibility, and restart-dependent
native portrait resolutions.

Standard V4L2 controls provide white balance, focus, anti-flicker, image settings, and format negotiation when the live
device advertises them. Every semantic mutation validates the current device/profile, honors automatic/manual
dependencies, reads back its result, and either restores the previous value or reports why rollback is unavailable.
Unknown firmware falls back to read-only or standard controls instead of inheriting unverified vendor writes.

Direct video supports exact advertised tuples, H.264/MJPEG pass-through, JPEG/PNG and raw snapshots, Matroska and MP4
recording, segmentation and rolling retention, disk guards, standard output, statistics, optional audio muxing, and
optional typed RTP/UDP output. Audio supports camera and explicitly selected third-party sources without rewriting
camera pickup controls.

Configuration and presets are strict, versioned, and validated before mutation. Direct operations use per-device
leases; `linkd` uses the same lease so a second physical capture stream is rejected rather than opened accidentally.
`doctor --bundle` already creates a private, redacted, checksummed diagnostic archive.

### Daemon and shared media graph

`linkd` is a per-user service with no network listener. Its socket lives below `$XDG_RUNTIME_DIR/linkctl`, the socket
and directory are owner-only, and both client and server verify the peer UID. Requests are serialized through a
bounded actor, including generic standard-control transactions and graph changes.

The media graph opens one physical `v4l2src`. Encoded data is teed to an optional recording branch; decoded raw video
is produced once and teed to dormant snapshot encoders and bounded virtual-output branches. Slow branches use small,
leaky downstream queues so they cannot accumulate unbounded latency. Direct snapshots and recordings automatically
route through the daemon when appropriate, while `--daemon never` keeps the direct path available.

The supervisor detects source errors and device removal, retries discovery with bounded exponential backoff, and
rebuilds the same runtime graph after the stable camera identity returns. Hardware validation confirmed recovery after
an unplug/replug without restarting the daemon: the process retained its PID, found the same stable ID on a different
video node, incremented its reconnect counter, and resumed frame delivery.

Graph and metrics commands expose the negotiated source and output contracts, queue bound and policy, processing
backend, source and per-output frames, bytes, drops, recent latency, bitrate, reconnect count, and the latest error.
The recent latency window contains 2,048 samples. The tested clean and transformed outputs measured approximately
37–39 ms p95 processing latency, below the current 150 ms target.

### Virtual-camera implementation

Named virtual outputs currently support explicit device, size, frame-rate, and raw-format contracts. Available base
operations include horizontal and vertical flip, rotation, normalized crop, contain/cover/stretch fitting, scale,
frame-rate conversion, color and format normalization, digital zoom and frame position, text/image overlay, and a
black privacy frame. Built-in output profiles are `clean`, `effects`, `mirrored`, and `portrait`; `effects` is currently
a clean foundation for future processing rather than an effects engine.

One physical 1080p30 stream successfully fed two virtual outputs with different dimensions and transforms. Two short
concurrent consumers received frames, captured output images showed the expected clean and mirrored/text variants,
and daemon snapshots and Matroska recording remained valid while both output branches were active.

That validation does not establish general virtual-camera compatibility. Reliable streaming consumers remain blocked
on the tested kernel-module combination as described below.

## v4l2loopback blocker

WebRTC applications require a loopback node that changes from output-only to capture-only while its producer is
streaming. v4l2loopback's `exclusive_caps=1` mode provides that behavior, and GStreamer's `v4l2sink` must therefore use
a streaming I/O mode rather than plain `write()` calls.

On the tested v4l2loopback 0.15.4 module, memory-mapped output reports errors such as:

```text
buffer 0 was not queued, this indicate a driver bug
Failed to allocate a buffer
streaming stopped, reason error (-5)
```

The producer can initially publish frames, but streaming consumers fail or corrupt the queue lifecycle, and a consumer
disconnect can propagate an error into the shared graph. The daemon then enters recovery because the GStreamer graph
cannot distinguish a broken kernel sink from a recoverable source failure. This behavior is tracked upstream in
[v4l2loopback #656](https://github.com/v4l2loopback/v4l2loopback/pull/656).

GStreamer's read/write sink mode is stable for read/write consumers, but it does not activate the v4l2loopback
streaming token. Under `exclusive_caps=1`, the node consequently remains output-only and Chromium/WebRTC will not
discover it as a camera. Disabling exclusive capabilities is also not a supported answer because applications that
reject mixed output/capture devices still will not expose it.

This is outside `linkctl`'s safe userspace boundary. Replacing the kernel module, carrying an unmerged kernel patch, or
implementing a custom V4L2 streaming sink would add system-level risk and a substantial maintenance burden. The first
release therefore keeps the implemented virtual-camera foundation, documents the affected environment, and does not
claim OBS or WebRTC compatibility.

The defect does not affect direct physical capture, file recording, snapshots, audio, camera controls, IPC, internal
raw processing, or file/network sinks. Those paths can continue to be used and maintained independently.

## Deferred feature inventory

### Productivity video workflows

The following host workflows are not included:

- Host portrait output with an explicit orientation and output contract.
- Smart Whiteboard automatic surface detection.
- Manual four-corner Whiteboard calibration.
- Perspective correction and document enhancement filters.
- Whiteboard freeze and rectified snapshot.
- Host DeskView calibration and perspective correction.
- Document mode.
- Versioned, per-device calibration persistence.
- Mutual-exclusion and transition rules between host modes.
- Simultaneous clean and processed virtual outputs.

The existing graph already supplies rotation, crop, scale, format normalization, overlays, snapshots, and multiple
named branches. It does not yet provide the calibration model, perspective warp, detection, enhancement pipeline, or
high-level mode state needed to turn those primitives into complete workflows.

These workflows can be developed against files or internal sinks, but their primary product surface is live processed
video in conferencing and recording applications. They are deferred as a coherent unit until that output can be
validated end to end rather than shipped as an offline-only approximation.

### Host framing and advanced effects

The following host-side vision and effects are not included:

- Person detection and tracking independent of camera-native Auto Framing.
- Single-person and group framing.
- Configurable crop smoothing, dead zone, and headroom.
- Regions of interest and exclusion zones.
- Optional active-speaker framing.
- Background segmentation.
- Background blur.
- Background replacement with an image or video.
- Chroma key and spill suppression.
- Foreground matting and edge smoothing.
- Natural or bokeh-style depth effects.
- Relighting.
- Filters and LUT processing.
- Optional skin smoothing, tone adjustment, face-aware exposure, subtle makeup overlays, and color filters.
- Optional host gesture recognition.
- Model/backend selection with explicit licensing metadata.
- CPU and GPU quality tiers.

The `host-ai` feature and `link-effects` crate reserve an architectural boundary only. No models are bundled or
downloaded, no detector backend is selected, and no effect command is exposed. The clean output remains the required
bypass path for any future implementation.

As with the productivity workflows, algorithms and offline fixtures are possible today. They are deferred because a
release-quality implementation must also prove stable live publication, bounded latency, deterministic missed-deadline
behavior, and immediate clean bypass in real consumers.

### Multi-camera, automation, and operations

The following operational work is deferred:

- Multiple-camera supervision within one daemon service.
- Coordinated per-camera and group preset operations with per-device results.
- A D-Bus facade.
- An optional local HTTP/WebSocket service.
- Pairing, expiring authentication, and event subscriptions for remote clients.
- Stream Deck examples.
- Optional MQTT, MIDI, and OSC adapters.
- Prometheus metrics.
- Hotkey integration examples.
- Configurable multi-camera health and recovery policies.

Some foundations already exist: direct CLI operations support stable device selection and per-device serialization;
device and control watches provide local event streams; presets support per-device configuration and deterministic
partial-failure reporting; the diagnostic bundle is private and redacted; and the single-camera daemon exposes local
metrics and hot-plug recovery.

Most work in this group is not technically blocked by v4l2loopback. It is intentionally deferred so the first release
does not freeze remote service APIs, authentication rules, or a multi-camera daemon model while the central live-output
path is unvalidated. Virtual-output health and multi-camera program feeds do directly depend on a reliable loopback
backend.

## Suggested implementation path after the blocker is resolved

### Re-establish the output baseline

Use a distribution or locally built v4l2loopback module whose output queue contains the upstream correction. Do not
accept a module based on its version string alone because distributions may backport or omit fixes. Record the kernel,
module source revision/package, GStreamer version, and consumer versions in the compatibility matrix.

Before adding features, exercise one clean output through repeated producer starts, consumer attach/detach cycles, and
producer restarts. Then repeat with two differently sized outputs, OBS, and a Chromium WebRTC page under
`exclusive_caps=1`. Require no allocator warnings, monotonically increasing output counters, bounded latency, and a
healthy daemon after each consumer closes.

### Add a typed processing boundary

Keep `link-media` responsible for capture, caps, tees, queue policy, and sinks. Implement host algorithms behind
`link-effects` using typed inputs and outputs rather than accepting arbitrary GStreamer strings. Each processing branch
should declare its input/output caps, queue bound, latency budget, model/backend identity, and clean-bypass behavior.

The shared graph should retain one decode and one raw tee. Attach processing branches downstream of bounded queues and
publish both clean and processed outputs from the same source. Extend existing per-output metrics with per-stage frame
time, missed deadlines, bypass state, and backend information.

### Build deterministic productivity modes first

Represent manual Whiteboard and DeskView calibration as normalized source coordinates plus a source-caps fingerprint.
Persist them in a strict versioned per-device document. Implement perspective warp and enhancement with deterministic
fixtures before adding automatic detection. Automatic Smart Whiteboard detection should produce the same calibration
shape and always retain the manual fallback.

Make the daemon actor own the active host mode and its calibration. Validate mutual exclusion before changing the
graph, build the replacement branch, switch only after it reaches the expected state, and fall back to the clean branch
or an explicit stopped state on failure. Host portrait should reuse the existing rotation and output-contract
primitives rather than introduce a separate capture path.

### Add framing and effects incrementally

Start with detector/tracker interfaces and recorded frame fixtures. Separate detection cadence from output cadence so
expensive inference cannot block frame delivery. Make crop smoothing, dead zones, headroom, lost-target behavior, and
group bounds deterministic and testable without a model.

Require users to select installed models or backends explicitly; never download a model silently. Add CPU quality tiers
before optional GPU backends, expose model license/provenance, and keep branch queues small and leaky. Implement
background and appearance effects as opt-in stages with the clean branch continuously available for immediate bypass.

### Scale the service and automation surface

Refactor daemon state into one supervisor, serialized control actor, and media actor per stable camera ID. Add a small
coordinator for group operations; it should validate the full request first and return a result for every camera rather
than hide partial failures.

Extend `link-ipc` with typed operations and events, preserving protocol version 1 where additions are compatible and
bumping the protocol only for incompatible wire changes. Build D-Bus as the first external facade. Any later HTTP or
WebSocket service should be opt-in, loopback-only by default, authenticated, origin-aware, and backed by expiring
pairing credentials stored by reference rather than inline in presets.

Keep Stream Deck, MQTT, MIDI, OSC, and hotkey integrations as clients of the stable facade rather than embedding them in
the daemon. Export metrics using non-secret stable IDs and preserve the current redaction rules in diagnostics.

## Re-entry acceptance checklist

Future development should not claim live host features ready until all of these checks pass:

- A corrected v4l2loopback build completes repeated streaming producer and consumer lifecycle tests without queue or
  allocator errors.
- Two named outputs with different caps run concurrently under `exclusive_caps=1`.
- OBS and Chromium/WebRTC can discover, open, close, and reopen each output.
- Clean and processed outputs survive consumer disconnects without rebuilding the physical source.
- Snapshot and recording remain valid while both outputs and processing are active.
- Hot unplug/replug restores the source, outputs, active mode, and metrics without restarting the daemon.
- Processing latency and missed deadlines are measured on every supported CPU/GPU tier.
- A clean bypass remains available when detection, a model, an accelerator, or an effect fails.
- The tested kernel/module/application combinations are recorded before release support is advertised.
