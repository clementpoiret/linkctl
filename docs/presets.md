# Configuration, presets, and transactions

Configuration is strict TOML with `schema_version = 1`. Unknown fields and unsupported schema versions are errors.
System configuration is loaded from `/etc/linkctl/config.toml`, followed by the user configuration at
`$XDG_CONFIG_HOME/linkctl/config.toml` or `~/.config/linkctl/config.toml`. Once one camera has been selected, an optional
`devices/<stable-id>.toml` layer may override `daemon`, `timeout`, and the `safety`, `media`, and `virtual_camera`
sections. Environment variables and explicit command-line arguments remain higher priority.

Per-device configuration supplies defaults only. Reading, selecting, or inspecting a camera never applies configured
state. Process-wide settings such as output format, logging, device selection, and profile directories are rejected in
per-device files.

## Preset catalog

User presets live under `$XDG_CONFIG_HOME/linkctl/presets/` or `~/.config/linkctl/presets/`. Names are safe filename
components containing ASCII letters, digits, dots, dashes, or underscores. Save and import never replace an existing
preset; delete requires the exact local name.

The application also exposes one immutable `builtin:default` preset. It is a safe linkctl baseline and explicitly is
not a claim about the complete vendor factory state. It selects normal camera mode; enables HDR; disables mirror,
flip, native portrait, and low-resolution compatibility; selects automatic exposure, white balance, and focus; sets
zero exposure compensation and 1× zoom; resolves live descriptor defaults for brightness, contrast, saturation, hue,
and sharpness; selects Standard pickup; and enables all three gestures. It deliberately preserves video format, audio
gain/mute, and anti-flicker.

```sh
linkctl preset list
linkctl preset show builtin:default
linkctl --dry-run preset apply builtin:default
linkctl preset export builtin:default default-template.toml

linkctl preset save interview --include camera,image,zoom,audio,gestures
linkctl preset show interview
linkctl --dry-run preset apply interview
linkctl preset apply interview
linkctl preset export interview interview.toml
linkctl preset import interview.toml
linkctl preset delete interview
```

`builtin:` cannot collide with a local preset name because colons are forbidden in local names. Built-ins can be
listed, shown, applied, and exported as editable templates; deletion is rejected.

Implemented capture groups are `video`, `camera`, `image`, `zoom`, `controls`, `audio`, and `gestures`. With no
`--include`, all groups are captured; `--exclude` is applied afterward. Capture reads camera-native values only from
an exact trusted verified profile. Raw standard controls use canonical V4L2 names and exact integer values so they
round-trip without normalized-slider loss. Pan and tilt are never captured and are rejected if requested.

## Semantic preset schema 2

Schema 2 has no schema-1 compatibility path: this project is pre-release, and an old document is rejected before any
write. Every absent field is preserved. Camera mode is one mutually exclusive value rather than independent booleans.
Framing style requires Auto Framing, and DeskView correction requires DeskView. Native portrait and low-resolution
compatibility cannot coexist. Restart-dependent changes require `[policy] allow_restart = true`.

```toml
schema_version = 2
name = "interview"
description = "Automatic half-body framing with Focus pickup"

[requirements]
model = "Insta360 Link 2C Pro"
usb_vid = 11802
usb_pid = 19461
fallback = "fail"

[camera]
mode = "auto-framing"
framing_style = "half-body"

[image]
hdr = true
exposure = "auto"
exposure_compensation_ev = 0.0
white_balance = "auto"
focus = "auto"
zoom = 1.0

[standard_controls]
brightness = "default"

[audio]
pickup_mode = "focus"

[gestures]
palm = true
v_sign = true
l_sign = true
```

Semantic image fields cover HDR, mirror/flip, automatic or manual ISO/shutter exposure, exposure compensation,
automatic or manual white balance and focus, zoom, and writable anti-flicker values. `[standard_controls]` remains
available for exact snapshots; a value may be an integer or `"default"`, which is resolved from the selected camera's
live descriptor during complete preflight. Audio pickup mode is independent from an optional explicit hardware or
host gain/mute layer.

## Application and recovery

Apply validates the complete document, model and optional USB IDs, exact trusted profile, live format, control ranges
and dependencies, audio source, restart policy, and rollback feasibility before issuing a write. The ordered plan is:
an optional combined camera restart, reversible camera-native controls, automatic/manual V4L2 prerequisites and
controls, audio gain/mute, and finally an explicitly requested video tuple. Profile-required probe streams restore the
previous tuple before the final video stage. Compatible read-modify-write fields sharing one selector are composed so
one field cannot erase another.

`--dry-run` reports the same plan without creating a journal or issuing device writes. A real apply takes a per-device
lease, snapshots readable state, verifies every stage, and rolls completed reversible stages back after failure.
Restart-dependent compatibility/native-portrait changes are explicitly irreversible and make rollback infeasible.

An in-progress report is atomically persisted under `$XDG_STATE_HOME/linkctl/transactions/` or
`~/.local/state/linkctl/transactions/`. It is removed after success or complete rollback. An incomplete rollback keeps
the journal, returns exit code 10, blocks another preset application to that camera, and appears as a warning in
`linkctl doctor` for explicit operator review.

Machine output continues to use envelope schema 1. Preset and preset-transaction documents use schema 2. The
normative JSON Schemas are in [schemas](schemas/).
