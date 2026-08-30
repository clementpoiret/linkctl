# ADR 0002: UVC Extension Unit access

**Status:** Accepted

## Context

Vendor controls require exact `UVCIOC_CTRL_QUERY` ABI layout, device-reported payload lengths, firmware guards, and a deliberately small unsafe boundary.

## Decision

`link-uvc-xu` implements a local wrapper with [`rustix::ioctl`](https://docs.rs/rustix/latest/rustix/ioctl/), an exact
`repr(C)` query structure, owned buffers, and borrowed file descriptors. Unsafe code is restricted to the ioctl call
under a focused lint exception and layout tests.

Read support must query `GET_INFO` and `GET_LEN` before `GET_CUR`. Semantic writes require an unforgeable capability minted from a compiled-in trusted verified profile and exact device guards. The raw transport method is compiled only with the non-default `research` feature; the CLI reaches it only after independent build, configuration, acknowledgement, exact profile, safety-class, length, stream-state, lease, and pacing checks. Neither route writes an unadvertised or unclassified selector.

## Consequences

The most sensitive ABI is not hidden behind an unrelated convenience layer. The wrapper carries an ongoing audit and native x86_64/AArch64 testing obligation. Normal builds retain the research command grammar while omitting its raw transport capability.
