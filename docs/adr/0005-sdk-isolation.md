# ADR 0005: Vendor SDK isolation

**Status:** Accepted

## Context

A future Link-specific SDK may have restrictive licensing, C++ ABI sensitivity, unsupported device coverage, or process stability problems. The public Desktop Camera SDK is not assumed to support Link webcams.

## Decision

`link-sdk-bridge` remains optional behind the `sdk` feature. A genuine, licensed Link-specific SDK will be discovered dynamically and loaded only by a helper process with a narrow C-compatible protocol. The portable core never links to or redistributes SDK headers or binaries.

Missing SDK files must leave all portable functionality available. SDK capabilities enter the same semantic capability model only after hardware tests.

## Consequences

Packaging works without proprietary files, and an SDK crash cannot directly crash the daemon. IPC and deployment complexity is accepted in exchange for license and fault isolation.
