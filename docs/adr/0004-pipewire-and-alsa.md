# ADR 0004: PipeWire with ALSA fallback

**Status:** Accepted

## Context

Modern Linux desktops expose routing and session policy through PipeWire, while headless and minimal systems may provide ALSA only.

## Decision

`link-audio` will use the [PipeWire Rust bindings](https://pipewire.pages.freedesktop.org/pipewire-rs/pipewire/) as the preferred session backend behind the `pipewire` feature. ALSA remains the direct fallback for discovery, hardware mixer access, and capture where PipeWire is absent or explicitly bypassed.

No audio library is linked until audio discovery is implemented.

## Consequences

The semantic audio API must report which backend supplied each value and distinguish hardware gain/mute from host processing. Device association tests must cover both backends.
