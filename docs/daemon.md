# Stream daemon and virtual cameras

`linkd` is a per-user daemon that owns one selected physical capture stream. It exposes a private, versioned Unix socket below `$XDG_RUNTIME_DIR/linkctl`, verifies the peer UID on every connection, and fans the decoded stream into bounded consumer branches. There is no network listener.

Start it directly while developing:

```sh
devenv shell -- cargo run -p link-daemon --bin linkd -- --device link2cpro-…
linkctl daemon status
linkctl pipeline graph
linkctl pipeline metrics
```

For a user service, install [the example unit](../packaging/systemd/linkd.service) as `~/.config/systemd/user/linkd.service`, ensure `linkd` is available on the service PATH, then run:

```sh
systemctl --user daemon-reload
systemctl --user enable --now linkd.service
```

`linkctl daemon reload` rebuilds the graph from the current camera and active output contracts. `daemon shutdown` gracefully finalizes an active recording, releases the physical source, and does not recreate runtime-only virtual outputs on the next start. `pipeline status`, `graph`, and `metrics` report source/output caps, branch queue bounds and policy, processing backend, frame and byte counters, per-output frames and queue drops, recent latency, bitrate, reconnect count, and the latest recovery error. Latency is measured at each output sink from GStreamer running time and buffer presentation time; `p95_latency_us` covers the most recent 2,048 delivered frames, and the clean 1080p30 release target is below `150000`. The supervisor retries a removed camera with bounded exponential backoff and rebuilds the same runtime branches when it reappears.

## Routing and ownership

The default `--daemon auto` policy uses `linkd` for snapshots and recording when its socket is present, otherwise those commands use their direct implementation. `--daemon always` requires a compatible daemon and returns exit code 12 if it is missing or uses another protocol version. `--daemon never` bypasses it. Direct stream commands refuse to open the physical camera while the daemon owns its media lease.

Daemon recording is intentionally background operation:

```sh
linkctl record start meeting.mkv --video-copy
linkctl record status
linkctl record stop
```

The shared recording branch currently supports one video-only Matroska or MP4 file without segmentation or duration/size limits. Use `--daemon never` for the richer blocking direct recorder. Snapshots briefly open dormant JPEG or PNG encoder branches and pull one frame without interrupting virtual-camera consumers; keeping their valves closed between requests avoids continuous encoder cost. Raw compressed snapshots remain direct-only.

Generic standard-control list/get/set/reset operations are routed through the daemon's serialized actor when it owns the selected camera. `control watch` remains a direct kernel event subscription, and `--daemon never` keeps the existing direct transaction path. Stream-dependent verified vendor reads and writes reuse the daemon's active physical stream while retaining their existing serialized control transaction; they do not create a second `v4l2src`.

## v4l2loopback setup

Install `v4l2loopback` using the distribution's kernel-module packaging. Module loading changes the host kernel and normally requires administrator privileges. For two outputs suitable for Chromium/WebRTC discovery:

```sh
sudo modprobe v4l2loopback devices=2 video_nr=20,21 \
  card_label="linkctl clean,linkctl effects" \
  exclusive_caps=1,1 max_buffers=8
```

`exclusive_caps=1,1` applies exclusive capabilities to both array entries: each node advertises output-only caps until its producer starts streaming and capture-only caps afterward, which improves Chromium/WebRTC compatibility. `max_buffers=8` gives GStreamer's memory-mapped producer enough loopback buffers for the two bounded branches. Secure Boot systems may require a signed module. Confirm the nodes before starting outputs:

```sh
v4l2-ctl --all --device /dev/video20
v4l2-ctl --all --device /dev/video21
```

`linkctl` requires a `v4l2loopback` build with correct V4L2 output-buffer queue semantics. Affected builds emit GStreamer's `buffer ... was not queued, this indicate a driver bug` message and streaming-I/O consumers then fail with `Failed to allocate a buffer`. This is a kernel-module defect tracked by [v4l2loopback #656](https://github.com/v4l2loopback/v4l2loopback/pull/656), not a recoverable virtual-camera branch error. Use a distribution module that does not exhibit the defect, or a module build containing the upstream fix, before validating OBS or WebRTC. GStreamer's read/write sink mode is not an equivalent workaround with `exclusive_caps=1`: it does not activate the streaming state needed for the node to advertise capture-only capabilities.

See [the current release state and deferred host features](release-state-and-deferred-features.md) for the validated component inventory, first-release boundary, and future implementation guidance.

The global `--device` always selects the physical source. `--output-device` names the loopback sink:

```sh
linkctl vcam start --name clean --output-device /dev/video20 --profile clean
linkctl vcam start --name conference --output-device /dev/video21 \
  --profile mirrored --size 1280x720 --fps 30 --text-overlay "On air"
linkctl vcam status
linkctl vcam stop --name conference
linkctl vcam stop
```

Built-in profiles are `clean`, `effects`, `mirrored`, and `portrait`; `effects` starts from a clean branch intended for explicit overlays and later effect controls. An output contract can also set rotation, horizontal/vertical flip, normalized crop, contain/cover/stretch fitting, zoom and frame position, text/image overlay, or a black privacy frame. Each branch normalizes raw format, dimensions, and frame rate before `v4l2sink`. LUT processing is not implemented.

Use `devenv --profile vcam-test shell` for the opt-in OBS and Chromium validation environment; those large GUI packages are excluded from the normal development shell. In OBS, add each node as a Video Capture Device. In Chromium, open a WebRTC camera test page and select the advertised loopback label. `pipeline metrics` should show increasing counters for both outputs while both consumers are active.
