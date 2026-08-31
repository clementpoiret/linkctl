# User guide

This guide covers the standard `linkctl` 1.0 build. It includes the local daemon, GStreamer, PipeWire, and ALSA, but
not the non-default research-write or network features. Run `linkctl --help` for the command surface compiled into the
installed binary.

## Install and verify access

### Debian, Fedora, and Arch Linux

Install the native package for the running distribution, reload the package's udev rule, and run the read-only doctor:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=video4linux
linkctl doctor
linkctl device list
```

### NixOS

Building or adding `linkctl` to a user profile does not register its udev rule or systemd user unit with NixOS. Add
the repository as a flake input and include its package in the system configuration. The following complete outline
uses `x86_64-linux`; use `aarch64-linux` on an AArch64 host and merge the input and module into an existing flake as
appropriate:

```nix
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  inputs.linkctl.url = "github:clementpoiret/linkctl";

  outputs = { nixpkgs, linkctl, ... }: {
    nixosConfigurations.your-host = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix
        ({ pkgs, ... }:
          let
            linkctlPackage = linkctl.packages.${pkgs.stdenv.hostPlatform.system}.linkctl;
          in {
            environment.systemPackages = [ linkctlPackage ];
            services.udev.packages = [ linkctlPackage ];
            systemd.packages = [ linkctlPackage ];
          })
      ];
    };
  };
}
```

Activate the system configuration and apply the new rule to a camera that is already connected:

```sh
sudo nixos-rebuild switch --flake .#your-host
sudo udevadm trigger --subsystem-match=video4linux
linkctl doctor
linkctl device list
```

`nixos-rebuild switch` installs the declared rule and restarts udev when the rule set changes, so a separate
`udevadm control --reload-rules` is normally redundant. The trigger is needed only to reprocess existing video nodes;
disconnecting and reconnecting the camera has the same effect. `nix build .#linkctl` remains useful for building or
inspecting the package, but it does not install these system integrations.

Both Nix-wrapped binaries use the complete GStreamer system-plugin path from the pinned package set, even when the
desktop session exports a different GStreamer version. The `gstreamer` line in `linkctl doctor` verifies the source,
caps-filter, and sink elements used by temporary camera-native status streams.

Normal commands run as the logged-in user. Do not use `sudo linkctl`; doing so changes XDG paths, daemon ownership,
and device permissions. If discovery succeeds but a camera node is inaccessible, follow [Device permissions](permissions.md).

`linkd` is optional. Enable it when snapshots, background recording, controls, and multiple output branches should
share one physical camera stream:

```sh
systemctl --user enable --now linkd.service
linkctl daemon status
```

## Select a camera

`linkctl device list` prints a non-secret stable ID for each discovered camera. Prefer that ID in configuration and
scripts. The `--device` selector also accepts an exact USB serial, `usb:<topology>`, `/dev/video*`, an unambiguous
`/dev/*/by-id` or `/dev/*/by-path` alias, or `index:<n>`. Bare numeric indexes are rejected because enumeration order
can change. Commands that explicitly support multiple devices accept `all`; mutations otherwise require one
unambiguous camera.

```sh
linkctl device list
linkctl --device link2cpro-… device info
linkctl --device usb:1-2.1 device probe
linkctl --device /dev/v4l/by-id/usb-… caps all
```

Set `default_device` in configuration or `LINKCTL_DEVICE` in the environment to omit `--device`. Device discovery and
probe output redact USB serials by default.

## Global options

Global options may appear before or after the subcommand:

| Option | Purpose |
|---|---|
| `-d, --device <selector>` | Select a camera |
| `--backend <auto|standard|vendor|host>` | Force an eligible control backend |
| `--daemon <auto|always|never>` | Prefer, require, or bypass `linkd` |
| `--format <human|json|jsonl>` | Select terminal, single-object, or event-stream output |
| `--timeout <duration>` | Set the operation deadline, such as `500ms` or `5s` |
| `--config <path>` | Replace the default user configuration file |
| `--profile-dir <path>` | Add an external read/research profile directory |
| `--log-level <off|error|warn|info|debug|trace>` | Set diagnostic verbosity |
| `--no-color` | Disable terminal color |
| `--dry-run` | Resolve and validate a mutation without applying it |
| `--yes` | Confirm commands that support non-interactive confirmation |
| `--unsafe-xu` | Supply one raw-XU research acknowledgement; insufficient by itself |
| `--schema-version <major>` | Require a supported machine-output schema |

Automatic backend selection uses standard Linux interfaces first, a trusted exact-match vendor profile second, and a
host implementation only for behavior explicitly identified as host-side. `--backend` cannot make an absent or unsafe
implementation available.

With `--daemon auto` (the default), commands use `linkd` when it owns the selected stream and supports the operation;
otherwise they use the direct path. `--daemon always` fails if the daemon is unavailable or incompatible.
`--daemon never` never contacts it.

## Configuration

Configuration uses strict TOML with `schema_version = 1`. Unknown keys, unknown `LINKCTL_*` variables, and unsupported
schema versions are errors. From lowest to highest priority, settings come from:

1. Built-in defaults.
2. `/etc/linkctl/config.toml`, if present.
3. The explicit `--config`/`LINKCTL_CONFIG` file, or otherwise
   `$XDG_CONFIG_HOME/linkctl/config.toml` (`~/.config/linkctl/config.toml`).
4. `$XDG_CONFIG_HOME/linkctl/devices/<stable-id>.toml`, after camera selection.
5. `LINKCTL_*` environment variables.
6. Command-line options.

An explicit config file is required to exist. A per-device file may override `daemon`, `timeout`, `safety`, `media`,
and `virtual_camera`; process-wide device, output, profile-directory, logging, and color settings are rejected there.
Configuration supplies defaults only: selecting or inspecting a device does not apply camera state.

```toml
schema_version = 1
default_device = "link2cpro-…"
daemon = "auto"
output = "human"
timeout = "3s"
log_level = "info"
no_color = false

[safety]
allow_raw_xu = false
minimum_xu_write_interval_ms = 250
allow_usb_reset = false
allow_driver_detach = false
redact_serials = true

[media]
preferred_transport = ["H264", "MJPG", "YUYV"]
default_container = "matroska"
disk_free_minimum = "5GiB"

[virtual_camera]
exclusive_caps_recommended = true
```

Safety booleans are policy inputs, not hidden feature switches. A normal build still blocks raw XU transport, USB
reset, and driver detach even if a file or environment variable requests them; configuration cannot enable code that
is absent or prohibited.

Common environment equivalents include `LINKCTL_DEVICE`, `LINKCTL_DAEMON`, `LINKCTL_FORMAT`, `LINKCTL_TIMEOUT`,
`LINKCTL_PROFILE_DIR`, `LINKCTL_LOG_LEVEL`, and `LINKCTL_NO_COLOR`. Nested settings use double underscores, for
example `LINKCTL_SAFETY__REDACT_SERIALS=false`. See [Configuration and presets](presets.md) for camera-state presets.

Preset documents use semantic schema 2. `linkctl preset list` includes the immutable `builtin:default` baseline and
local files from `~/.config/linkctl/presets/`; inspect its complete transaction with
`linkctl --dry-run preset apply builtin:default` before applying it. Export the built-in to obtain an editable user
template.

## Output and exit codes

Human output is intended for terminals. `--format json` emits one schema-1 envelope; `--format jsonl` emits one
envelope per event or result. Every envelope has `schema_version`, `ok`, `command`, `device`, `result`, and `error`.
On failure, `error` includes a stable `code`, numeric `exit_code`, message, and structured details. Binary media on
standard output never contains logs or JSON; diagnostics remain on standard error.

The public process exit codes are:

| Code | Meaning |
|---:|---|
| 0 | Success |
| 2 | Invalid invocation or configuration |
| 3 | Device not found |
| 4 | Capability unsupported or unmapped |
| 5 | Device or resource busy |
| 6 | Permission denied |
| 7 | Device or filesystem I/O failure |
| 8 | Protocol or profile mismatch |
| 9 | Unsafe operation denied |
| 10 | Partial success; inspect transaction recovery details |
| 11 | Timeout |
| 12 | Daemon unavailable or incompatible |
| 13 | Media pipeline failure |
| 14 | Firmware-staging validation failure |

Scripts should test the exit code and `ok`, then branch on `error.code`; human-readable messages may improve without a
schema change. The normative output schema is [envelope-v1.json](schemas/envelope-v1.json).

## Common workflows

Inspect first, then mutate with a dry run when the command changes state:

```sh
linkctl --device link2cpro-… caps all
linkctl --device link2cpro-… control list
linkctl --device link2cpro-… image status
linkctl --device link2cpro-… --dry-run control set brightness 55%
linkctl --device link2cpro-… control set brightness 55%
linkctl --device link2cpro-… zoom ramp 1.0x 1.5x --duration 750ms
```

Negotiate an exact live-advertised video tuple, take a snapshot, or record with optional camera audio:

```sh
linkctl --device link2cpro-… video formats --fourcc H264
linkctl --device link2cpro-… video set --fourcc H264 --size 1920x1080 --fps 60
linkctl --device link2cpro-… snapshot frame.png
linkctl --device link2cpro-… record start meeting.mkv --video-copy --audio camera
```

Save reproducible standard state and inspect the complete plan before applying it:

```sh
linkctl --device link2cpro-… preset save interview --include video,image,zoom,audio
linkctl --device link2cpro-… --dry-run preset apply interview
linkctl --device link2cpro-… preset apply interview
```

Watch commands use JSON Lines well:

```sh
linkctl --format jsonl device watch
linkctl --device link2cpro-… --format jsonl control watch
linkctl --device link2cpro-… --format jsonl audio meter
```

See [Controls](controls.md), [Media](media.md), [Audio](audio.md), [Daemon and virtual cameras](daemon.md), and
[Firmware maintenance](firmware.md) for operation-specific constraints.

## Mutations, privacy, and recovery

Mutations validate inputs and prerequisites, serialize access per camera, read back the result, and attempt rollback
when the control can be restored. Restart-dependent compatibility and native-portrait changes wait for the same camera
to re-enumerate, rematch its exact profile, and verify the new state. `--dry-run` performs resolution and validation
without writing.

`privacy status` is read-only. It reports what can be established about stream activity and hardware/host audio mute,
but the Link 2C Pro exposes no verified shutter-position sensor and `linkctl` has no logical privacy enter/exit command.
Treat an unknown shutter value as unknown, not closed. Stop applications or `linkd` when a guaranteed logical stop is
required, and use the physical shutter for a physical privacy boundary.

If a command fails:

1. Run `linkctl doctor` and inspect its GStreamer, permission, configuration, profile, daemon, and recovery-journal
   checks.
2. Use `--daemon never` to distinguish a direct-device issue from daemon ownership, or `--daemon always` to require
   the service path.
3. Close other camera applications when the device is busy; `linkctl` deliberately avoids opening a second physical
   stream behind another owner.
4. For shareable diagnostics, run `linkctl doctor --bundle report.tar.zst`. The no-clobber archive is owner-only and
   redacted, but review it before sharing.
5. If bounded XU recovery fails, stop writing and physically reconnect the camera. Do not detach `uvcvideo` or reset
   the USB device through ad hoc privileged commands.

For release-supported hardware and known environmental limitations, consult the [compatibility matrix](compatibility.md)
and [feature status](feature-status.md).
