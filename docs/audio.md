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

On the verified Link 2C Pro firmware profile, these commands use XU GUID `e307e649-4618-a3ff-82fc-2d8b5f216773`, selector 31, as a one-byte enum: Standard is `0`, Wide is `1`, Focus is `2`, and Original is `3`. The Controller capture repeated the `Original → Focus → Wide → Standard` sequence and ended in Focus, with later `GET_CUR` samples confirming every written value.

Linux verification completed three `Original → Focus → Wide → Standard → Focus` cycles with the video stream closed. Each command reads the previous value, performs the Controller's double-`GET_LEN` prelude, writes the enum, waits 250 milliseconds, verifies direct readback, and restores the previous mode on mismatch. Mutations completed in 0.52–0.66 seconds. Status and mutations remain camera-native and do not substitute a PipeWire filter. Reconnect and power-cycle persistence remain unverified because Focus is both the capture's final value and the observed default.

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
