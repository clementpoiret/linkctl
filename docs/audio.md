# Audio discovery, control, and capture

`linkctl` enumerates system audio endpoints and merges ALSA PCMs with their PipeWire nodes. The camera microphone is correlated to its video device through USB and ALSA metadata; unrelated microphones remain selectable by stable audio ID without changing any camera setting.

```sh
linkctl audio devices
linkctl --device link2cpro-… audio status
linkctl audio status --source audio-…
linkctl audio status --source alsa:hw:4,0
linkctl audio status --source pipewire:alsa_input.usb-…
```

`audio devices` reports capture and playback routes, sample ranges/formats, discovered mixer controls, default-session routing, exclusive ALSA busy state, and current gain/mute layers. Machine output keeps hardware and host state separate.

Camera-resident pickup modes have a separate semantic surface:

```sh
linkctl --device link2cpro-… audio mode status
linkctl --device link2cpro-… audio mode standard
linkctl --device link2cpro-… audio mode wide
linkctl --device link2cpro-… audio mode focus
linkctl --device link2cpro-… audio mode original
```

These commands do not substitute a PipeWire filter for a camera mode. Until an exact transport and firmware-specific mapping are verified, status reports `discovered-unmapped` and mutations fail without issuing a device write.

## Gain and mute

Automatic control prefers a working ALSA/UAC hardware mixer and falls back to the PipeWire session node. Force the layer with global `--backend standard` or `--backend host`:

```sh
linkctl --device link2cpro-… audio gain 70%
linkctl --device link2cpro-… audio mute
linkctl --device link2cpro-… audio unmute
linkctl --device link2cpro-… --backend host audio gain 80%
```

Writes read the previous value, apply the request, verify readback, and attempt rollback if verification fails. `--dry-run` resolves the source and requested layer without changing it. Hardware mute and PipeWire host mute are independent; `audio status` reports both and an effective combined mute.

## Capture and metering

The PipeWire route is preferred when available; an ALSA-only build or explicit `alsa:` source uses direct ALSA capture. Capture converts to signed 16-bit interleaved PCM at the selected rate/channel count. Output extension selects WAV, FLAC, or headerless raw PCM:

```sh
linkctl --device link2cpro-… audio capture mic.wav --duration 30s
linkctl audio capture interview.flac --source audio-…
linkctl audio capture samples.raw --sample-rate 16000 --channels 1
linkctl audio capture --stdout --audio-format wav | consumer
linkctl --format jsonl audio meter --duration 10s --interval 200ms
```

Binary standard output never contains diagnostics. Meter events report peak/RMS dBFS, clipping, elapsed time, and discontinuities; the final report also includes buffer/byte and resampler drop/add counters. A broken downstream pipe is a normal stop.

`--gate`, `--compressor`, and `--limiter` enable fixed conservative host filters for capture, meter, monitor, or recording. They are opt-in; the default branch performs only the conversion/resampling required by the requested output.

## Monitoring and recording

Monitoring uses the current session sink unless another stable playback ID or explicit route is selected. Keep the requested latency high enough for the hardware and avoid acoustic feedback:

```sh
linkctl audio monitor --latency 50ms
linkctl audio monitor --sink pipewire:alsa_output.pci-… --duration 10s
```

Recording remains video-only unless `--audio` is supplied. Matroska uses FLAC and MP4 uses AAC. The report includes A/V offset, maximum offset, drift, sample correction, clipping, and timestamp discontinuities:

```sh
linkctl record start meeting.mkv --audio camera
linkctl record start interview.mp4 --audio audio-… --audio-delay=-25ms
linkctl record start processed.mkv --audio camera --gate --compressor --limiter
```

`--audio-delay` changes the audio timestamp by a signed duration. `audiorate` performs bounded timestamp/sample correction and exposes its added/dropped sample counters in the result.
