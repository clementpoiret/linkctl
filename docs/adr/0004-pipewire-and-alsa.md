# ADR 0004: PipeWire with ALSA fallback

**Status:** Accepted

## Context

Modern Linux desktops expose routing and session policy through PipeWire, while headless and minimal systems may provide ALSA only.

## Decision

`link-audio` uses the [PipeWire Rust bindings](https://pipewire.pages.freedesktop.org/pipewire-rs/pipewire/) as the preferred session backend behind the `pipewire` feature. ALSA remains the direct fallback for discovery, hardware mixer access, and capture where PipeWire is absent or explicitly bypassed.

Normal CLI builds enable PipeWire. Discovery merges PipeWire nodes and ALSA PCMs into logical endpoints using ALSA card metadata, then correlates camera microphones with the camera's USB identity. Direct ALSA mixer state and PipeWire session state are reported separately. Host gain/mute writes use typed PipeWire properties with readback and bounded rollback.

## Consequences

The semantic audio API reports which backend supplied each value and distinguishes hardware gain/mute from host processing. PipeWire is session-scoped and may be unavailable in a headless environment; commands continue to expose ALSA routes and controls in that case. Device association tests cover route merging and explicit non-camera source selection.
