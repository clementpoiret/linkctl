# Media capture and recording

`linkctl` enumerates the selected capture node before using it. It never synthesizes an advertised product mode: FourCC, dimensions, and frame rate must match a live discrete or stepwise V4L2 range.

```sh
linkctl --device /dev/video2 video formats
linkctl --device /dev/video2 video status
linkctl --device /dev/video2 video set --fourcc H264 --size 3840x2160 --fps 30
linkctl --device /dev/video2 video stats --fourcc H264 --size 1920x1080 --fps 60 --duration 10s
```

`video set` applies the format and frame interval, reads both back, and restores the previous tuple if the driver adjusts the request. Global `--dry-run` validates enumeration and `VIDIOC_TRY_FMT` without changing the device.

## Snapshots

The output extension selects JPEG or PNG. Binary stdout requires an explicit image encoding because global `--format` remains reserved for human/JSON output.

```sh
linkctl snapshot frame.jpg
linkctl snapshot frame.png --count 5 --interval 1s
linkctl snapshot - --image-format png > frame.png
linkctl snapshot raw.mjpg --raw-frame --fourcc MJPG
```

File snapshots create `<output>.json` metadata by default. It records the redacted device identity, exact applied tuple, matched profile, timestamp, and readable standard controls. Use `--no-metadata` to omit it. Existing outputs are rejected unless `--overwrite` is explicit.

Direct media commands take a per-device advisory lock. `linkd` takes the same lock for its shared graph. With the default `--daemon auto`, snapshots use the daemon when it is running and do not interrupt its recording or virtual-camera branches. A direct snapshot attempted while another owner holds the camera fails with `device-busy` without opening a second stream.

## Recording and pipes

With no daemon, foreground recording blocks until its duration/size limit, Ctrl-C, a disk guard, or an error stops it. Video-only recording remains the default. `--audio` opts in to a microphone source without changing camera controls. When `linkd` is running, `record start` adds a background pass-through branch; inspect and finalize it with `record status` and `record stop`. Use `--daemon never` when the direct recorder's audio, segmentation, rolling, duration, or size options are required.

```sh
linkctl record start meeting.mkv --video-copy
linkctl record start meeting.mkv --video-copy --audio camera
linkctl record start clip.mp4 --audio audio-… --audio-delay=-25ms
linkctl record start clip.mp4 --duration 30s
linkctl record start rolling.mkv --segment-duration 5m --rolling 6
linkctl capture --video h264 --stdout | consumer
```

Matroska is the default container. MP4 uses short fragments and is finalized on EOS. A successful single-file recording is atomically renamed from a same-directory temporary path; an abnormal interruption may leave a clearly named `.linkctl-part-*` recovery candidate. Segmented recordings use `name-00000.ext` siblings. Rolling mode deletes only older siblings generated for that explicit recording prefix.

Matroska audio is encoded as FLAC; MP4 audio is encoded as AAC. The audio branch converts channel layout and sample rate, uses monotonic timestamps, and applies `audiorate` correction. The final report includes audio buffers/bytes, clipping and discontinuity counts, dropped/added samples, levels, and measured A/V offset and drift. `--audio-delay` applies a signed timestamp offset. `--gate`, `--compressor`, and `--limiter` enable fixed conservative host presets and are disabled by default.

See [audio](audio.md) for source selection, standalone capture, monitoring, and gain/mute behavior.

The configured `media.disk_free_minimum` defaults to `5GiB`. Recording checks it before starting and while running. `--disk-reserve`, `--max-size`, `--segment-size`, and the configuration accept bytes plus decimal `KB/MB/GB` or binary `KiB/MiB/GiB` units.

Binary stdout contains media bytes only. Human or JSON completion/error diagnostics are written to stderr, and a downstream broken pipe is a clean stop.

## RTP/UDP

Build with `--features network` to send H.264 or MJPEG RTP to an explicit destination:

```sh
linkctl stream start --host 127.0.0.1 --port 5004 \
  --fourcc H264 --size 1920x1080 --fps 30
```

The command opens no listener and performs no implicit network discovery. H.264 defaults to dynamic payload type 96; MJPEG uses its static payload type unless `--payload-type` is supplied.
