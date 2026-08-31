# linkctl

`linkctl` is an independent Linux command-line tool and per-user service for the fixed-mount Insta360 Link 2C Pro.
It provides the controls that Linux exposes through UVC, V4L2, UAC, ALSA, and PipeWire, plus carefully guarded
camera-native controls learned from a lawfully operated device. Insta360 documents Linux UVC/UAC compatibility but
does not provide Link Controller for Linux; `linkctl` fills that interoperability gap.

This project is unofficial. It is not affiliated with, sponsored by, or endorsed by Insta360. See the
[legal and clean-room notice](docs/legal.md) before contributing proprietary-protocol research.

## What works

The standard 1.0 build includes:

- Stable device discovery, live capability inspection, hotplug events, diagnostics, and shell completions.
- Standard image controls, exact video-format negotiation, snapshots, H.264/MJPEG capture, and Matroska/MP4
  recording.
- PipeWire and ALSA audio discovery, gain/mute, capture, metering, monitoring, pickup modes, and optional A/V muxing.
- Semantic camera presets with an immutable `builtin:default`, local user definitions, dry runs, readback, rollback,
  and recovery journals.
- A per-user daemon that stays camera-idle until needed, then owns one shared stream for snapshots, recording, metrics,
  and named raw virtual-output branches.
- Verified Link 2C Pro controls for Auto Framing, HDR, mirror/flip, exposure, Whiteboard, DeskView, gestures,
  low-resolution compatibility, native portrait, and firmware information on the exact tested firmware/profile.
- Read-only Extension Unit research tools and a bounded workflow for staging an official, user-supplied firmware file.

Host vision/effects, remote listeners, multi-camera daemon supervision, and logical privacy transitions are not
included. OBS/WebRTC virtual-camera use is not supported with the tested `v4l2loopback` 0.15.4 module. The complete,
evidence-backed boundary is in [Feature status](docs/feature-status.md).

## Install and start

Release artifacts provide native packages for Debian 13, Fedora 44, Arch Linux, and Arch Linux ARM, plus a pinned
flake package for NixOS 26.05, on the architectures listed in the [compatibility matrix](docs/compatibility.md). Each
package contains `linkctl`, `linkd`, a hardened systemd user unit, a narrow udev rule, manuals, licenses, checksummed
profiles, documentation, and completion scripts. Installation does not enable or start the daemon automatically.

After installing a Debian, Fedora, or Arch package:

```sh
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=video4linux
linkctl doctor
linkctl device list
```

On NixOS, add the flake package to the system environment and register the same package with both udev and systemd:

```nix
let
  linkctlPackage = linkctl.packages.${pkgs.stdenv.hostPlatform.system}.linkctl;
in {
  environment.systemPackages = [ linkctlPackage ];
  services.udev.packages = [ linkctlPackage ];
  systemd.packages = [ linkctlPackage ];
}
```

Here `linkctl` is the repository's flake input. Apply the configuration with `nixos-rebuild switch`, then reconnect
the camera or run `sudo udevadm trigger --subsystem-match=video4linux` if it is already connected. A standalone
`nix build .#linkctl` builds the package but does not register its udev rule or systemd user unit. See the
[NixOS installation instructions](docs/user-guide.md#nixos) for a complete flake example.

The Nix package gives both binaries one complete, pinned GStreamer system-plugin path instead of inheriting a
potentially incompatible desktop path. `linkctl doctor` verifies the core elements required for camera-native status
reads before reporting the installation healthy.

Normal operation is unprivileged. Administrative access is needed only to install or remove a native package, activate
a NixOS system configuration, and refresh udev rules when required. The daemon is optional:

```sh
systemctl --user enable --now linkd.service
linkctl daemon status
```

Keeping the service enabled has negligible idle cost: `linkd` blocks on IPC without opening the camera or constructing
a media graph until a background recording or virtual output is active. See the [daemon power and decoder
controls](docs/daemon.md#power-and-decoder-policy) for active-stream tuning.

Verify downloaded artifacts using `SHA256SUMS` and the GitHub artifact attestation described in the
[release runbook](docs/release-runbook.md).

## First commands

Start with the stable ID printed by `device list`; examples below abbreviate it as `link2cpro-…`:

```sh
linkctl device list
linkctl --device link2cpro-… device info
linkctl --device link2cpro-… caps all
linkctl --device link2cpro-… image status
linkctl --device link2cpro-… video formats
linkctl --device link2cpro-… audio status
linkctl --device link2cpro-… snapshot frame.png
```

Mutations validate the live device, support `--dry-run`, and verify their result:

```sh
linkctl --device link2cpro-… --dry-run image exposure manual --shutter 1/120 --iso 400
linkctl --device link2cpro-… image exposure manual --shutter 1/120 --iso 400
linkctl --device link2cpro-… auto-framing on
linkctl --device link2cpro-… record start meeting.mkv --video-copy --audio camera
linkctl --device link2cpro-… --dry-run preset apply builtin:default
linkctl --device link2cpro-… preset save interview --include camera,image,zoom,audio,gestures
linkctl --device link2cpro-… --dry-run preset apply interview
```

Use `--format json` for one versioned result or `--format jsonl` for event streams. Run `linkctl --help` and
`linkctl <command> --help` for the authoritative command surface.

## Documentation

| Guide | Contents |
|---|---|
| [User guide](docs/user-guide.md) | Installation, device selection, configuration, output, exit codes, workflows, and troubleshooting |
| [Feature status](docs/feature-status.md) | Supported, conditional, experimental, unavailable, and prohibited behavior |
| [Compatibility](docs/compatibility.md) | Distributions, architectures, camera firmware, media paths, and applications |
| [Architecture](docs/architecture.md) | Backends, direct/daemon data flow, crate boundaries, IPC, profiles, and safety |
| [Controls](docs/controls.md) and [camera-native capabilities](docs/camera-native.md) | Standard and verified device controls |
| [Media](docs/media.md), [audio](docs/audio.md), and [daemon](docs/daemon.md) | Capture, recording, monitoring, shared streams, and virtual outputs |
| [Configuration and presets](docs/presets.md) | Strict TOML, local presets, transactions, and recovery |
| [Permissions](docs/permissions.md) and [hardware probe](docs/hardware-probe.md) | Linux access setup and diagnostic evidence |
| [Firmware](docs/firmware.md) and [XU research](docs/xu-research.md) | Guarded maintenance and research procedures |
| [Legal notice](docs/legal.md) | Independence, interoperability analysis, trademark use, and contribution provenance |
| [Threat model](docs/threat-model.md) and [security policy](SECURITY.md) | Trust boundaries, mitigations, and reporting |
| [Upgrade guide](docs/upgrade.md) and [release runbook](docs/release-runbook.md) | Operator upgrades and maintainer release procedure |

## Safety model

Standard controls come from the live V4L2 descriptors. Vendor writes require a compiled-in reviewed profile matching
the exact device identity, descriptor fingerprint, firmware, selector, and payload length. Unknown firmware falls back
to standard controls and safe reads. Mutations read back their outcome and attempt rollback where the operation permits
it.

Normal builds cannot issue raw Extension Unit writes. Driver detach, USB reset, forced bootloader entry, calibration,
flash, and mechanical writes are prohibited. Firmware staging never bundles or downloads firmware and never mounts,
unmounts, or disconnects the camera; it accepts only an explicit official file and the camera's manually entered
U-Disk mode. See [Feature status](docs/feature-status.md) for the exact boundary.

## Development

The development environment is managed by [devenv](https://devenv.sh/):

```sh
devenv shell
rustc --version  # 1.97.1
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo deny --all-features check
cargo audit
```

The normal environment supplies GStreamer and PipeWire dependencies. `devenv --profile vcam-test shell` additionally
provides OBS and Chromium for opt-in desktop interoperability checks. See [CONTRIBUTING.md](CONTRIBUTING.md) and the
[architecture decisions](docs/adr/) before changing a device or media boundary.

## Legal and license

The repository review found no Insta360 firmware, controller binary, source code, artwork, model, credential, or raw
packet capture distributed by this project. That is an engineering provenance finding, not a legal opinion. Laws and
contract terms differ by jurisdiction; contributors and distributors are responsible for their own compliance. Read
the full [legal and clean-room notice](docs/legal.md).

`linkctl` is available under your choice of the [MIT license](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE). Those licenses cover this project's code and documentation only; they do not
grant rights to third-party software, firmware, trademarks, or media.
