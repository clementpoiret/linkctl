# ADR 0003: GStreamer media graph

**Status:** Accepted

## Context

Capture fan-out, recording, transforms, virtual cameras, and network outputs require a mature Linux media graph.

## Decision

`link-media` uses the [official GStreamer Rust bindings](https://gstreamer.freedesktop.org/documentation/rust/stable/latest/docs/gstreamer/)
behind the `gstreamer` feature. Pipelines are built programmatically from typed requests. Untrusted clients and
configuration cannot provide arbitrary pipeline strings.

Normal builds include the media backend and local daemon client; `--no-default-features` retains a compile-only build without the native backend. The devenv supplies GStreamer 1.28 core, base, good, bad, and libav plugins. Programmatic audio graphs provide capture, metering, monitoring, fixed optional DSP, resampling, and A/V muxing. `linkd` holds one source, tees encoded data to recording, decodes once, and tees raw frames to snapshot and bounded virtual-output branches. The `network` feature adds typed RTP/UDP output without accepting arbitrary pipeline strings.

## Consequences

Native dependencies remain feature-gated. Reconfiguring outputs rebuilds one typed graph and briefly interrupts every branch; runtime output contracts are retained across hotplug recovery but not an intentional daemon restart. Every supported pipeline requires caps-negotiation, bus-error, shutdown, and latency tests.
