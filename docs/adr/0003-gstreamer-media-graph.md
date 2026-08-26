# ADR 0003: GStreamer media graph

**Status:** Accepted

## Context

Capture fan-out, recording, transforms, virtual cameras, and network outputs require a mature Linux media graph.

## Decision

`link-media` will use the [official GStreamer Rust bindings](https://gstreamer.freedesktop.org/documentation/rust/stable/latest/docs/gstreamer/) behind the `gstreamer` feature. Pipelines will be built programmatically from typed requests. Untrusted clients and configuration will not provide arbitrary pipeline strings.

The feature currently establishes dependency direction only; native GStreamer packages are added with the first media implementation.

## Consequences

Native dependencies remain optional, while future stream ownership and fan-out share one graph. Every supported pipeline requires caps-negotiation, bus-error, shutdown, and latency tests.
