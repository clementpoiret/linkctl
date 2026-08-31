# Architecture

`linkctl` separates standard Linux device access, exact-profile vendor controls, and host media processing. That
separation keeps runtime discovery authoritative and prevents a feature learned from one firmware or camera model from
silently becoming a write claim for another.

## System view

```text
linkctl CLI
  |-- direct mode ----------------------------+
  |                                           |
  +-- protocol 1 over owner-only Unix socket  |
            |                                 |
          linkd                               |
            |-- idle IPC/control actor --------+
            +-- active shared source ----------+--> encoded recording branch
                                               \--> gated decode --> snapshots / raw outputs

Linux discovery --> V4L2/UVC video and controls
                --> PipeWire/ALSA audio
                --> sysfs/udev USB identity and U-Disk detection

Trusted exact-match profile --> typed, guarded camera-native reads/writes
```

The CLI is always the user-facing contract. Direct mode opens the selected device for the duration of one operation.
Daemon mode sends a versioned request to the per-user `linkd` service, which owns one camera stream and serializes
supported controls and graph changes. There is no network listener.

## Capability layers

1. **Standard device layer.** Discovery reads sysfs/udev and groups USB, V4L2, media-controller, ALSA, and maintenance
   nodes. Formats and controls come from live V4L2 descriptors. Audio routing comes from PipeWire when available with
   ALSA as the direct fallback.
2. **Verified vendor layer.** Extension Unit descriptors are parsed at runtime. Typed vendor behavior is enabled only
   when a compiled-in reviewed profile matches the camera mode, VID/PID, USB revision, descriptor SHA-256, firmware,
   GUID, selector, and live payload length. External profiles can improve read/research decoding but cannot grant
   semantic write authority.
3. **Host layer.** GStreamer implements capture, conversion, muxing, bounded fan-out, and base transforms. Host
   behavior is reported as host-side and never presented as a camera-native capability.

Automatic backend selection prefers standard controls, then an eligible verified profile, then an implemented host
backend. An explicit `--backend` narrows that choice; it cannot bypass a capability or safety guard.

## Workspace boundaries

| Component | Responsibility |
|---|---|
| `link-core` | Public domain types, configuration, safety policy, output envelope, presets, transactions, firmware reports |
| `link-linux` | USB identity, stable selectors, sysfs/udev discovery, hotplug, node association, U-Disk discovery |
| `link-v4l2` | Live V4L2 format/control inventory, negotiation, capture-node status, typed standard writes and rollback |
| `link-audio` | PipeWire/ALSA discovery, association, gain/mute, capture, metering, monitoring, resampling, basic DSP |
| `link-profiles` | Strict profile loading/matching, typed codecs, evidence metadata, stream and write policies |
| `link-uvc-xu` | Descriptor parsing and exact `UVCIOC_CTRL_QUERY` access behind a focused audited unsafe boundary |
| `link-media` | Typed GStreamer capture, snapshots, recording, audio muxing, pipes, shared graph, optional RTP/UDP |
| `link-ipc` | Length-bounded protocol-1 JSON/binary framing and same-user peer authentication |
| `link-daemon` | Per-user socket, one-camera supervision, serialized controls, shared source, recovery, graph metrics |
| `link-cli` | Command parsing, orchestration, human and machine output, confirmation and dry-run behavior |
| `link-testkit` | Redacted recorded probes and hardware-free fixtures |

Higher layers use project-owned types rather than exposing kernel or GStreamer structures as public contracts.
Pipelines are constructed from typed requests; configuration and IPC cannot inject arbitrary GStreamer strings or
shell commands.

## Direct and daemon data flow

The default `--daemon auto` policy uses the daemon when it owns the selected camera and implements the requested
operation. Otherwise the command takes a per-device lease and uses a direct backend. `--daemon always` requires the
service; `--daemon never` requires the direct path. The same lease namespace prevents accidental competing physical
streams.

`linkd` keeps no physical `v4l2src` open while idle. It creates one shared source while at least one persistent
recording or virtual-output consumer exists, and the last consumer releases the graph and media lease immediately; a
forced idle snapshot uses a bounded transient source instead. Encoded input can pass directly to a recording branch;
the raw side remains gated during recording-only operation and is decoded once when a snapshot or raw output needs it.
Closed snapshot valves precede their queues. Recording and output branches are added or removed without restarting the
source, and each has a bounded queue with an explicit backpressure policy. Automatic H.264/MJPEG decode uses an
accessible VA-API render node when available and otherwise uses software. On unplug, the supervisor uses the stable
camera identity, bounded backoff, and the last exact tuple to rebuild the graph. An interrupted recording continues in
the next unused
`<stem>.reconnect-NNN.<ext>` file rather than overwriting a finalized segment.

IPC lives below `$XDG_RUNTIME_DIR/linkctl` in an owner-only directory and socket. Client and server verify peer user
IDs, frames are length-bounded, and incompatible protocol versions fail closed. Machine output, IPC, presets,
transactions, profiles, and XU snapshots each carry an explicit schema or protocol version.

## Trust and safety boundaries

Device and configuration inputs are untrusted. Strict parsers reject unknown fields and malformed sizes. Read-only
inventory never changes formats or controls; a stream-dependent status read may briefly hold the current tuple without
changing it. Mutations resolve the complete request before writing, enforce value ranges and prerequisites, verify
readback, and attempt bounded rollback.

Normal builds omit raw XU write transport. No path detaches the kernel driver, resets USB, forces bootloader entry, or
writes firmware, calibration, flash, or mechanical controls. Firmware maintenance copies one explicit official file
to an already mounted, exact-match maintenance volume using no-clobber and post-copy hash checks. See the
[threat model](threat-model.md), [feature status](feature-status.md), and [legal notice](legal.md).

## Architectural decisions

- [ADR 0001](adr/0001-v4l2-access.md): V4L2 access boundary.
- [ADR 0002](adr/0002-uvc-xu-access.md): focused UVC Extension Unit ioctl boundary.
- [ADR 0003](adr/0003-gstreamer-media-graph.md): typed GStreamer media graph.
- [ADR 0004](adr/0004-pipewire-and-alsa.md): PipeWire with ALSA fallback.
