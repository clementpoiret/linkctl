# ADR 0003: GStreamer media graph

**Status:** Accepted

## Context

Capture fan-out, recording, transforms, virtual cameras, and network outputs require a mature Linux media graph.

## Decision

`link-media` uses the [official GStreamer Rust bindings](https://gstreamer.freedesktop.org/documentation/rust/stable/latest/docs/gstreamer/)
behind the `gstreamer` feature. Pipelines are built programmatically from typed requests. Untrusted clients and
configuration cannot provide arbitrary pipeline strings.

Normal builds include the media backend and local daemon client; `--no-default-features` retains a compile-only build
without the native backend. The devenv supplies GStreamer 1.28 core, base, good, bad, and libav plugins. Programmatic
audio graphs provide capture, metering, monitoring, fixed optional DSP, resampling, and A/V muxing. `linkd` opens no
source while it has no persistent consumer. While active it tees encoded data to recording and tees decoded raw frames
to bounded snapshot and virtual-output branches. Consumer branches are attached and detached in place behind closed
valves, so normal output or recording reconfiguration does not restart the physical source. Snapshot valves precede
their queues and encoders; the raw/decode side is gated during recording-only operation. Automatic decode prefers a
usable VA-API element and falls back to software. The `network` feature adds typed RTP/UDP output without accepting
arbitrary pipeline strings.

## Consequences

Native dependencies remain feature-gated. Runtime output contracts are retained across hotplug recovery but not an
intentional daemon restart. Source recovery and explicit reload still rebuild the graph; ordinary consumer changes do
not. Dynamic recording removal requires bounded EOS finalization before the branch is detached. Every supported
pipeline requires caps-negotiation, bus-error, shutdown, dynamic-branch, and latency tests.
