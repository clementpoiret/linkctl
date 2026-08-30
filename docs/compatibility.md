# Compatibility matrix

This document is the support contract for the standard package. A target is supported only for the feature boundary
described here; optional research, network, and host-processing builds are not release configurations.

## Distribution packages

Packages are built inside their target distribution so that ELF dependencies come from that distribution rather than
from the development Nix store.

| Distribution | Package | x86-64 | AArch64 | Runtime baseline |
|---|---|---:|---:|---|
| Debian 13 | `.deb` | supported | supported | Linux 6.12, GStreamer 1.26.2, systemd user session |
| Fedora 44 | `.rpm` | supported | supported | Distribution kernel and GStreamer 1.28 |
| Arch Linux | `.pkg.tar.zst` | supported | n/a | Current repositories, GStreamer 1.26 or newer |
| Arch Linux ARM | `.pkg.tar.zst` | n/a | supported | Current aarch64 repositories, GStreamer 1.26 or newer |
| NixOS 26.05 | flake package | supported | supported | Pinned nixpkgs plus Rust 1.97.1 from the pinned Rust overlay |

The source requires Rust 1.97.1. The runtime GStreamer floor is 1.26; core, base, good, bad, and libav plugins are part
of the package contract. PipeWire is preferred in a desktop session and ALSA remains available as a direct fallback.
`linkd` is usable without systemd when started directly.

AArch64 receives the same locked build, parser/ABI tests, and package checks as x86-64. Camera hardware validation is
currently performed on x86-64; this distinction is not a claim of AArch64 camera validation.

## Camera and firmware

| Device state | Firmware / identity | Status |
|---|---|---|
| Landscape camera | Link 2C Pro `2e1a:4c05`, `v0.2.9.8_build3` | verified read/write profile |
| Low-resolution compatibility personality | recorded descriptor for the same firmware | verified restart and restore |
| Native portrait personality | recorded descriptor for the same firmware | verified restart, tuples, snapshot, and restore |
| U-Disk maintenance personality | recorded USB and volume identity | verified detection only; no firmware was staged |
| Any other firmware or descriptor | unmatched | standard controls and safe reads only; vendor writes refused |
| Link 2, Link 2 Pro, or other Insta360 camera | different identity | discovery may work; no inherited writable profile |

The verified profile covers camera-native Auto Framing and Head/Half-body styles, HDR, horizontal mirror, vertical
flip, scalar exposure, four microphone pickup modes, regular Whiteboard, DeskView with vertical correction, three
gesture switches, low-resolution compatibility, and native portrait. White balance, focus, anti-flicker, ordinary
image controls, and zoom use live standard V4L2 controls where advertised.

## Media and applications

| Path | Tested boundary | Status |
|---|---|---|
| Direct capture, snapshots, H.264/MJPEG pass-through, Matroska/MP4 recording | 30-minute 4K30 H.264 and 60-minute 1080p60 H.264 hardware runs | supported |
| Camera and selected external audio, PipeWire/ALSA, FLAC/AAC muxing | 60-minute 1080p60 plus 48 kHz mono FLAC hardware run | supported |
| Local daemon IPC and one-camera shared graph | owner-only Unix socket; snapshot and recording hotplug recovery | supported |
| Multiple internal raw virtual outputs | explicit v4l2loopback nodes | implemented |
| OBS and Chromium/WebRTC virtual-camera use | v4l2loopback 0.15.4 | unsupported: affected by streaming queue failure |
| RTP/UDP | non-default `network` build | experimental; absent from standard packages |
| RTSP, SRT, integrated WebRTC, remote HTTP/WebSocket/MQTT | none | unavailable |

The v4l2loopback limitation does not affect the physical camera, file outputs, local snapshots, recordings, audio,
controls, or daemon IPC. No package installs or loads v4l2loopback automatically.

## Explicitly unavailable standard-package features

- Host Auto Framing, Smart Whiteboard, host DeskView/document correction, and host portrait orchestration.
- Background segmentation, blur/replacement, chroma key, relighting, face/appearance processing, or bundled models.
- Multi-camera supervision in one daemon.
- Remote control listeners or cloud services.
- Logical privacy enter/exit; `privacy status` remains an honest read-only separation of physical and logical state.
