# linkctl

`linkctl` is a Linux command-line controller for the fixed-mount Insta360 Link 2C Pro. The project is capability-driven: it uses standard Linux camera and audio interfaces first, verified device profiles second, and explicitly labelled host-side processing where appropriate.

The CLI discovers cameras by stable USB identity, reports their live capabilities, controls standard V4L2 image settings, and captures media through exact runtime-enumerated video tuples:

```sh
linkctl device list
linkctl --device link2cpro-… device info
linkctl --device link2cpro-… caps controls
linkctl --device link2cpro-… control list
linkctl --device link2cpro-… control set brightness 55%
linkctl --device link2cpro-… image white-balance 5000K
linkctl --device link2cpro-… video formats --fourcc H264
linkctl audio devices
linkctl --device link2cpro-… audio status
linkctl --device link2cpro-… audio meter --format jsonl
linkctl --device link2cpro-… snapshot frame.png
linkctl --device link2cpro-… record start meeting.mkv --video-copy --audio camera
```

Every mutation supports `--dry-run`, validates values, reads the previous value, verifies readback, and attempts rollback after a partial failure. Automatic/manual prerequisites are applied by default; `control set --raw` bypasses only that semantic gating. Pan and tilt remain read-only raw inventory even if a driver advertises them.

Use `device watch --format jsonl` for hotplug events and `control watch --format jsonl` for control changes. `linkctl doctor` performs read-only configuration, permission, profile, and control checks. See [audio](docs/audio.md), [video capture and recording](docs/media.md), [standard controls](docs/controls.md), [permissions and udev setup](docs/permissions.md), and the [hardware probe guide](docs/hardware-probe.md).

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

Normal builds expose validated standard V4L2 control writes. They do not expose raw Extension Unit writes, driver detach, USB reset, firmware or calibration writes, or mechanical movement commands. Configuration cannot enable code that is absent from the build. A profile is never sufficient evidence for a vendor write until it has been validated against the exact device, descriptor, and firmware under a separately reviewed implementation.

Discovery, watches, capability reports, probes, and `doctor` are read-only. They never set a V4L2 format or control and never issue a UVC `SET_CUR` request.

Machine output uses schema version 1. JSON and JSON Lines errors always include `schema_version`, `ok`, `command`, `device`, `result`, and `error`.

See [CONTRIBUTING.md](CONTRIBUTING.md), the [threat model](docs/threat-model.md), and the [architecture decisions](docs/adr/) for the engineering contract.
