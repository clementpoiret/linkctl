# ADR 0001: V4L2 access boundary

**Status:** Accepted

## Context

Camera formats and extended controls require broad V4L2 coverage without exposing raw kernel structures throughout the application.

## Decision

`link-v4l2` will own V4L2 access and initially use the thin ioctl layer from [`v4l2r`](https://docs.rs/v4l2r/) for enumeration, controls, formats, and queues. Higher layers consume project-owned domain types rather than `v4l2r` types. Missing operations may receive a small local wrapper after ABI review; the work-in-progress high-level `v4l2r` device API is not a required application abstraction.

No V4L2 dependency is linked until device behavior is implemented and tested with fixtures.

## Consequences

Kernel-facing changes remain auditable and replaceable. The backend must account for generated-header requirements and must test ioctl structure layout on each supported architecture.
