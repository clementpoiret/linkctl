# ADR 0002: UVC Extension Unit access

**Status:** Accepted

## Context

Vendor controls require exact `UVCIOC_CTRL_QUERY` ABI layout, device-reported payload lengths, firmware guards, and a deliberately small unsafe boundary.

## Decision

`link-uvc-xu` will implement a local wrapper with [`rustix::ioctl`](https://docs.rs/rustix/latest/rustix/ioctl/), an exact `repr(C)` query structure, owned buffers, and borrowed file descriptors. Unsafe code will be restricted to the ioctl call after the crate receives a focused lint exception and layout tests.

The wrapper will provide no unknown write operation. Read support must query `GET_INFO` and `GET_LEN` before `GET_CUR`; semantic writes require verified profile authorization and exact device guards.

## Consequences

The most sensitive ABI is not hidden behind an unrelated convenience layer. The wrapper carries an ongoing audit and cross-architecture testing obligation.
