# linkctl

`linkctl` is a Linux command-line controller for the fixed-mount Insta360 Link 2C Pro. The project is capability-driven: it uses standard Linux camera and audio interfaces first, verified device profiles second, and explicitly labelled host-side processing where appropriate.

The CLI discovers cameras by stable USB identity, reports their live capabilities, controls standard V4L2 image settings, and captures media through exact runtime-enumerated video tuples:

```sh
linkctl device list
linkctl --device link2cpro-… device info
linkctl --device link2cpro-… device state
linkctl --device link2cpro-… caps all
linkctl --device link2cpro-… caps controls
linkctl --device link2cpro-… control list
linkctl --device link2cpro-… control set brightness 55%
linkctl --device link2cpro-… image white-balance auto
linkctl --device link2cpro-… image white-balance 5000K
linkctl --device link2cpro-… image hdr on
linkctl --device link2cpro-… zoom set 1.5x
linkctl --device link2cpro-… auto-framing status
linkctl --device link2cpro-… auto-framing on
linkctl --device link2cpro-… auto-framing style half-body
linkctl --device link2cpro-… firmware info
linkctl --device link2cpro-… video formats --fourcc H264
linkctl audio devices
linkctl --device link2cpro-… audio status
linkctl --device link2cpro-… audio meter --format jsonl
linkctl --device link2cpro-… snapshot frame.png
linkctl --device link2cpro-… record start meeting.mkv --video-copy --audio camera
linkctl --device link2cpro-… preset save interview --include video,image,zoom,audio
linkctl --device link2cpro-… --dry-run preset apply interview
linkctl --device link2cpro-… xu inventory
linkctl --device link2cpro-… xu snapshot baseline.json --samples 5
linkctl xu diff baseline.json after.json
```

Every mutation supports `--dry-run`, validates values, reads the previous value, verifies readback, and attempts rollback after a partial failure. Automatic/manual prerequisites are applied by default; `control set --raw` bypasses only that semantic gating. Pan and tilt remain read-only raw inventory even if a driver advertises them.

Use `device watch --format jsonl` for hotplug events and `control watch --format jsonl` for control changes. `linkctl doctor` performs read-only configuration, permission, profile, control, and recovery-journal checks; `doctor --bundle report.tar.zst` creates a private redacted diagnostic archive. See [camera-native capabilities](docs/camera-native.md), [configuration and presets](docs/presets.md), [audio](docs/audio.md), [video capture and recording](docs/media.md), [standard controls](docs/controls.md), [safe XU research](docs/xu-research.md), [permissions and udev setup](docs/permissions.md), and the [hardware probe guide](docs/hardware-probe.md).

GStreamer and PipeWire support are enabled in normal builds, with direct ALSA capture as a fallback. H.264 and MJPEG recording paths preserve the camera encoding without decoding; recording audio is opt-in and muxes FLAC into Matroska or AAC into MP4. RTP/UDP output is available when the `network` feature is enabled.

Generate shell completion source with `linkctl completion bash`, `zsh`, `fish`, or `elvish` and load or install the result using the conventions of that shell.

## Development

The development environment is managed by [devenv](https://devenv.sh/):

```sh
devenv shell
cargo test --workspace --all-features --locked
cargo run -p link-cli --bin linkctl -- --help
```

The devenv includes PipeWire and the GStreamer core, base, good, bad, and libav plugins required for camera/audio capture, monitoring, snapshot decoding, container muxing, and optional RTP output.

Before submitting a change, run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo deny --all-features check
```

## Safety boundary

Normal builds expose validated standard V4L2 control writes and safe Extension Unit reads. `caps all` reports camera-native items as standard, verified-profile, hardware-only, or explicitly unmapped; unavailable vendor features are never inferred from older Link models. The raw XU command remains visible but cannot issue `SET_CUR` unless the binary was built with the non-default `research` feature and every runtime/profile gate passes. Driver detach, USB reset, firmware, calibration, and mechanical writes remain prohibited. Configuration cannot enable code that is absent from the build. A semantic vendor write additionally requires a compiled-in trusted verified profile matched to the exact device, descriptor, and firmware.

For the recorded landscape descriptor on firmware `v0.2.9.8_build3`, Auto Framing status, on/off mutations, Head/Half-body styles, and HDR are provided by a verified profile. White-balance auto/manual mode and temperature use the camera's standard UVC controls and are validated from the live V4L2 descriptors. Auto Framing's enabled-state selector reports its active value only while video is streaming, so status briefly opens a no-output stream at the current video tuple before reading it. Guarded writes use the captured 1920×1080 MJPEG stream conditions with a hardware-validated one-second warm-up and 500-millisecond delayed readback, validate selector lengths, and roll back on mismatch. HDR and Smart Composition share a selector, so each command changes only its verified bit in a freshly read value. A style command transactionally enables the camera's Smart Composition prerequisite before setting the style; it does not implicitly enable Auto Framing itself.

Discovery, read watches, capability reports, probes, snapshots, diffs, and `doctor` are device-read-only. They never change a V4L2 format or control and never issue a UVC `SET_CUR` request; stream-dependent reads may briefly hold the current tuple without changing it. An explicitly requested probe, snapshot, or diagnostic artifact is the only filesystem output.

Machine output uses schema version 1. JSON and JSON Lines errors always include `schema_version`, `ok`, `command`, `device`, `result`, and `error`.

Local preset files and per-device configuration are strict, versioned TOML. Preset application resolves and validates the full plan before writing, serializes direct operations per device, verifies each stage, and retains a recovery journal only when rollback cannot fully restore the previous state.

See [CONTRIBUTING.md](CONTRIBUTING.md), the [threat model](docs/threat-model.md), and the [architecture decisions](docs/adr/) for the engineering contract.
