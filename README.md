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
linkctl --device link2cpro-… image exposure manual --shutter 1/120 --iso 400
linkctl --device link2cpro-… image exposure-compensation +0.7
linkctl --device link2cpro-… image white-balance auto
linkctl --device link2cpro-… image white-balance 5000K
linkctl --device link2cpro-… image focus auto
linkctl --device link2cpro-… image focus manual 0.5
linkctl --device link2cpro-… image anti-flicker 50hz
linkctl --device link2cpro-… image hdr on
linkctl --device link2cpro-… image mirror on
linkctl --device link2cpro-… image flip on
linkctl --device link2cpro-… zoom set 1.5x
linkctl --device link2cpro-… auto-framing status
linkctl --device link2cpro-… auto-framing on
linkctl --device link2cpro-… auto-framing style half-body
linkctl --device link2cpro-… mode deskview vertical-correction 45
linkctl --device link2cpro-… mode compatibility status
linkctl --device link2cpro-… mode compatibility set low-resolution
linkctl --device link2cpro-… portrait native enable
linkctl --device link2cpro-… portrait native disable
linkctl --device link2cpro-… firmware info
linkctl --device usb:10-3 firmware watch
linkctl --device usb:10-3 --dry-run firmware stage ./Insta360LINK2CPROFW_HOST.bin
linkctl --device usb:10-3 --yes firmware stage ./Insta360LINK2CPROFW_HOST.bin --sha256 <official-sha256>
linkctl --device link2cpro-… video formats --fourcc H264
linkctl audio devices
linkctl --device link2cpro-… audio status
linkctl --device link2cpro-… audio mode focus
linkctl --device link2cpro-… audio meter --format jsonl
linkctl --device link2cpro-… snapshot frame.png
linkctl --device link2cpro-… record start meeting.mkv --video-copy --audio camera
linkctl daemon status
linkctl pipeline metrics
linkctl vcam start --name clean --output-device /dev/video20 --profile clean
linkctl --device link2cpro-… preset save interview --include video,image,zoom,audio
linkctl --device link2cpro-… --dry-run preset apply interview
linkctl --device link2cpro-… xu inventory
linkctl --device link2cpro-… xu snapshot baseline.json --samples 5
linkctl xu diff baseline.json after.json
```

Every mutation supports `--dry-run`, validates values, and verifies its declared outcome. Ordinary controls read the previous value and attempt rollback after a partial failure; restart-dependent compatibility changes instead wait for the camera to disappear and return with an exact profile match and post-restart state. Automatic/manual prerequisites are applied by default; `control set --raw` bypasses only that semantic gating. Pan and tilt remain read-only raw inventory even if a driver advertises them.

Use `device watch --format jsonl` for hotplug events and `control watch --format jsonl` for control changes. While `linkd` owns the selected camera, generic standard-control list/get/set/reset operations use its serialized control actor; `--daemon never` selects the direct path. `linkctl doctor` performs read-only configuration, permission, profile, control, and recovery-journal checks; `doctor --bundle report.tar.zst` creates a private redacted diagnostic archive. See the [current release state and deferred host features](docs/release-state-and-deferred-features.md), [camera-native capabilities](docs/camera-native.md), [configuration and presets](docs/presets.md), [audio](docs/audio.md), [video capture and recording](docs/media.md), [the stream daemon and virtual cameras](docs/daemon.md), [firmware maintenance](docs/firmware.md), [standard controls](docs/controls.md), [safe XU research](docs/xu-research.md), [permissions and udev setup](docs/permissions.md), and the [hardware probe guide](docs/hardware-probe.md).

GStreamer, PipeWire, and the local daemon client are enabled in normal builds, with direct ALSA capture as a fallback. `linkd` owns one physical GStreamer source and fans it out to snapshots, a background recording, and multiple named `v4l2loopback` outputs; graph and metrics output includes bounded-queue policy, per-output drops, and recent p95 processing latency. Direct H.264 and MJPEG recording paths preserve the camera encoding without decoding; recording audio is opt-in and muxes FLAC into Matroska or AAC into MP4. RTP/UDP output is available when the `network` feature is enabled.

Generate shell completion source with `linkctl completion bash`, `zsh`, `fish`, or `elvish` and load or install the result using the conventions of that shell.

## Development

The development environment is managed by [devenv](https://devenv.sh/):

```sh
devenv shell
cargo test --workspace --all-features --locked
cargo run -p link-cli --bin linkctl -- --help
cargo run -p link-daemon --bin linkd -- --help
```

The normal devenv includes PipeWire and the GStreamer core, base, good, bad, and libav plugins required for camera/audio capture, monitoring, snapshot decoding, container muxing, virtual-camera publication, and optional RTP output. `devenv --profile vcam-test shell` additionally provides OBS and Chromium for opt-in desktop interoperability checks.

Before submitting a change, run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo deny --all-features check
```

## Safety boundary

Normal builds expose validated standard V4L2 control writes and safe Extension Unit reads. `caps all` reports camera-native items as standard, verified-profile, hardware-only, or explicitly unmapped; unavailable vendor features are never inferred from older Link models. The raw XU command remains visible but cannot issue `SET_CUR` unless the binary was built with the non-default `research` feature and every runtime/profile gate passes. Driver detach, USB reset, firmware/boot device controls, calibration, and mechanical writes remain prohibited. The separately reviewed firmware workflow only stages an explicitly supplied official file onto the exact mounted U-Disk filesystem, with no device-control write, automatic mount, unmount, or disconnect. Configuration cannot enable code that is absent from the build. A semantic vendor write additionally requires a compiled-in trusted verified profile matched to the exact device, descriptor, and firmware.

For the recorded landscape, Low resolution, and native portrait descriptors on firmware `v0.2.9.8_build3`, Auto Framing status, on/off mutations, Head/Half-body styles, HDR, horizontal mirroring, vertical flipping, camera-native scalar exposure, all four microphone pickup modes, regular camera-resident Whiteboard, the camera's DeskView mode, its three gesture switches, restart-dependent 360p compatibility mode, and restart-dependent native portrait resolution are provided by a verified profile. Exposure supports automatic/manual mode, ISO 100–3200, shutter values from 1/8000 through 1/30, and exposure compensation from -3.0 through +3.0 EV in 0.1 EV steps; curve mutation remains unavailable because its captured write-only protocol cannot be read back or rolled back. White-balance, focus, and anti-flicker controls use the camera's standard UVC controls and are validated from the live V4L2 descriptors. The target Linux driver exposes anti-flicker as disabled, 50 Hz, or 60 Hz; although the Controller capture also identifies raw value 3 as automatic, the live descriptor does not advertise that value and `linkctl` rejects it before writing. Audio pickup mode is an independent one-byte XU enum that is readable and writable without opening the video stream. Regular Whiteboard is the camera-native `mode whiteboard` transform; Smart Whiteboard remains a separate host/virtual-camera workflow. DeskView on/off and its 10–80 vertical-correction setting use the camera-native processor; they do not replace the separate host perspective-correction and virtual-camera pipeline. The Palm, V-sign, and L-sign switches respectively govern the camera's Auto Framing, Whiteboard, and Zoom gestures; global enable/disable changes all three while individual settings preserve the others. Auto Framing, Whiteboard, DeskView, and gesture status use the daemon's active stream when available, otherwise they briefly open a no-output stream at the current video tuple before reading their stream-dependent selectors. Guarded writes use captured stream conditions, validate selector lengths, and roll back on mismatch where the profile declares rollback available. HDR, Smart Composition, horizontal mirror, vertical flip, low-resolution compatibility, native portrait, and the gesture settings use masked read-modify-write encoding to change only each feature's verified bits in a freshly read value. A style command transactionally enables the camera's Smart Composition prerequisite before setting the style; it does not implicitly enable Auto Framing itself. Changed compatibility or native-portrait values cause the camera's own restart; the command waits for re-enumeration, rematches the exact descriptor and firmware profile, then verifies the new state. Native portrait exposes 736×1280, 1080×1920, 1088×1920, and 2176×3840 tuples; physical camera rotation is still required for official portrait operation. Native portrait and Low resolution are mutually exclusive until their combined descriptor is verified. The portrait correction switches remain unmapped.

Discovery, read watches, capability reports, probes, snapshots, diffs, and `doctor` are device-read-only. They never change a V4L2 format or control and never issue a UVC `SET_CUR` request; stream-dependent reads may briefly hold the current tuple without changing it. Filesystem output is limited to explicitly requested artifacts and the confirmed firmware-staging workflow. Firmware staging uses no-clobber copy semantics, synchronization, post-copy hashing, and private operation logs.

Machine output uses schema version 1. JSON and JSON Lines errors always include `schema_version`, `ok`, `command`, `device`, `result`, and `error`.

Local preset files and per-device configuration are strict, versioned TOML. Preset application resolves and validates the full plan before writing, serializes direct operations per device, verifies each stage, and retains a recovery journal only when rollback cannot fully restore the previous state.

See [CONTRIBUTING.md](CONTRIBUTING.md), the [threat model](docs/threat-model.md), and the [architecture decisions](docs/adr/) for the engineering contract.
