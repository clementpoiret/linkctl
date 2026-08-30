# ADR 0001: V4L2 access boundary

**Status:** Accepted

## Context

Camera formats and extended controls require broad V4L2 coverage without exposing raw kernel structures throughout the application.

## Decision

`link-v4l2` owns V4L2 access and uses the thin ioctl layer from [`v4l2r`](https://docs.rs/v4l2r/) for enumeration,
controls, formats, and queues. Higher layers consume project-owned domain types rather than `v4l2r` types. Operations
not covered by that layer use a small local wrapper only after ABI review; the high-level `v4l2r` device API is not an
application abstraction.

## Consequences

Kernel-facing changes remain auditable and replaceable. The backend accounts for generated-header requirements and
tests ioctl structure layout on each supported architecture.
