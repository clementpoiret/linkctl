# ADR 0003: GStreamer media graph

**Status:** Accepted

## Context

Capture fan-out, recording, transforms, virtual cameras, and network outputs require a mature Linux media graph.

## Decision

`link-media` will use the [official GStreamer Rust bindings](https://gstreamer.freedesktop.org/documentation/rust/stable/latest/docs/gstreamer/) behind the `gstreamer` feature. Pipelines will be built programmatically from typed requests. Untrusted clients and configuration will not provide arbitrary pipeline strings.

Normal builds include the media backend; `--no-default-features` retains a compile-only build without the native backend. The devenv supplies GStreamer 1.28 core, base, good, bad, and libav plugins. The `network` feature adds typed RTP/UDP output without accepting arbitrary pipeline strings.

## Consequences

Native dependencies remain feature-gated, while future stream ownership and fan-out can reuse the same typed requests and statistics. Every supported pipeline requires caps-negotiation, bus-error, shutdown, and latency tests.
