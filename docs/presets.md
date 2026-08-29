# Configuration, presets, and transactions

Configuration is strict TOML with `schema_version = 1`. Unknown fields and unsupported schema versions are errors.
System configuration is loaded from `/etc/linkctl/config.toml`, followed by the user configuration at
`$XDG_CONFIG_HOME/linkctl/config.toml` or `~/.config/linkctl/config.toml`. Once one camera has been selected, an optional
`devices/<stable-id>.toml` layer may override `daemon`, `timeout`, and the `safety`, `media`, and `virtual_camera`
sections. Environment variables and explicit command-line arguments remain higher priority.

Per-device configuration supplies defaults only. Reading, selecting, or inspecting a camera never applies configured
state. Process-wide settings such as output format, logging, device selection, and profile directories are rejected in
per-device files.

## Local presets

Presets live under `$XDG_CONFIG_HOME/linkctl/presets/` or `~/.config/linkctl/presets/`. Names are safe filename
components containing ASCII letters, digits, dots, dashes, or underscores. Save and import never replace an existing
preset; delete requires the exact preset name.

```sh
linkctl preset save interview --include video,image,zoom,audio
linkctl preset list
linkctl preset show interview
linkctl --dry-run preset apply interview
linkctl preset apply interview
linkctl preset export interview interview.toml
linkctl preset export interview -
linkctl preset import interview.toml
linkctl preset delete interview
```

Implemented capture groups are `video`, `image`, `zoom`, `controls`, and `audio`. With no `--include`, all implemented
groups are captured; `--exclude` is applied afterward. Standard controls use their canonical V4L2 names and exact raw
integer values so values round-trip without normalized-slider loss. Pan and tilt are never captured and are rejected if
an imported preset requests them.

Preset schema 1 intentionally contains only state that current direct backends can verify: an exact video tuple, safe
standard controls (including standard digital zoom), and one explicit hardware or host audio-control layer. Camera-native
vendor modes are not captured until a trusted profile can read, write, verify, and restore them; effects,
recording/streaming targets, and inline credentials are also not accepted. No earlier public preset schema exists;
loading still passes through a version-dispatch boundary so a real migration can be added when one is needed.

```toml
schema_version = 1
name = "interview"
description = "Reproducible local camera state"

[requirements]
model = "Insta360 Link 2C Pro"
usb_vid = 11802
usb_pid = 19461
fallback = "fail"

[video]
fourcc = "MJPG"
width = 3840
height = 2160
fps_num = 30
fps_den = 1

[standard_controls]
brightness = 50
contrast = 50
white_balance_automatic = 1
zoom_absolute = 100

[audio]
source = "camera"
layer = "hardware"
gain_percent = 75.0
mute = false
```

## Application and recovery

Apply validates the complete document, selected model and optional USB IDs, forced backend, exact live format,
control ranges/dependencies, audio source, and rollback feasibility before issuing a write. The transaction order is
video format, automatic/manual control prerequisites, remaining standard controls, audio gain, then audio mute.
Verified no-ops are skipped.

`--dry-run` reports the same ordered plan without creating a journal or opening a writable device handle. A real apply
takes a per-device lease, snapshots readable state, verifies every stage, and rolls completed stages back in reverse
order after failure. The lease is shared with direct format, control, audio, capture, snapshot, recording, and streaming
operations; another operation waits up to the configured timeout before returning `device-busy`.

An in-progress report is atomically persisted under `$XDG_STATE_HOME/linkctl/transactions/` or
`~/.local/state/linkctl/transactions/`. It is removed after success or complete rollback. An incomplete rollback keeps
the journal, returns exit code 10, blocks another preset application to that camera, and appears as a warning in
`linkctl doctor` for explicit operator review.

Machine output continues to use envelope schema 1. The normative JSON Schemas are in [schemas](schemas/).
