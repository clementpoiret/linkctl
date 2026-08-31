# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.2] - 2026-08-31

### Changed

- Made `linkd` release the camera and block without polling while idle, dynamically attach recording and virtual-output
  branches, gate recording-only decode, and prefer usable VA-API decoders with software fallback.
- Reduced repeated USB discovery and watch overhead by caching one-shot command discovery, hydrating descriptors only
  for relevant USB devices, reusing XU watch sessions, and consuming V4L2 control events directly.

### Fixed

- Accepted the documented `LINKCTL_DAEMON_SOCKET` transport override (and the `linkd`-only decoder override) without
  treating either as an unknown persistent configuration variable.

## [1.0.1] - 2026-08-31

### Fixed

- Included GStreamer's core elements in the Nix package, isolated both wrappers from incompatible desktop system-plugin
  paths, and made `doctor` report missing elements required by camera-native status reads.
- Read aggregate camera-native capability values in compatible passive and open-stream batches so restart-dependent
  controls no longer abort `caps all`.

## [1.0.0] - 2026-08-30

### Added

- Capability-driven Link 2C Pro discovery, stable selection, diagnostics, and redacted probe bundles.
- Standard V4L2 image controls and exact video tuple negotiation.
- Verified, firmware-guarded camera-native controls for Auto Framing, HDR, mirror/flip, exposure, audio pickup modes,
  Whiteboard, DeskView, gestures, compatibility mode, and native portrait.
- ALSA and PipeWire audio discovery, control, capture, monitoring, metering, processing, and A/V muxing.
- Snapshots, direct and daemon-owned recording, local pipes, optional RTP output, and a bounded shared media graph.
- Versioned local IPC, transactional presets, rollback journals, safe XU research tools, and manual firmware staging.
- Debian, Fedora/RPM, Arch/Arch Linux ARM, and NixOS package definitions with a hardened per-user service, udev rules,
  manuals, and Bash/Zsh/Fish/Elvish completions.
- Reproducible source and binary checks, CycloneDX SBOM generation, release manifests, checksums, and prepared Sigstore
  artifact attestation.

### Security

- Writable vendor mappings are restricted to compiled-in verified profiles; external profiles remain non-authoritative
  for semantic writes.
- Normal builds exclude raw XU writes, host AI, and network listeners.

### Fixed

- Isolated live video and audio recording branches with bounded queues and measured their stream clocks independently,
  preventing muxer backpressure and callback ordering from distorting long-duration capture and A/V drift reports.
- Preserved the active source tuple across daemon camera recovery and resumed an interrupted background recording in a
  deterministic, non-overwriting `.reconnect-NNN` sibling.

### Known limitations

- Host computer-vision modes, background/appearance effects, multi-camera daemon supervision, remote control, and
  logical privacy transitions are unavailable.
- Virtual-camera graph production exists, but OBS and WebRTC compatibility is unsupported with the tested v4l2loopback
  0.15.4 queue behavior.

[1.0.2]: https://github.com/clementpoiret/linkctl/releases/tag/v1.0.2
[1.0.1]: https://github.com/clementpoiret/linkctl/releases/tag/v1.0.1
[1.0.0]: https://github.com/clementpoiret/linkctl/releases/tag/v1.0.0
[unreleased]: https://github.com/clementpoiret/linkctl/compare/v1.0.2...HEAD
