# Threat model

## Security objectives

The system must preserve camera availability, prevent unverified device writes, keep local media and identifiers private, and ensure that only the logged-in user can control future long-running services. A successful command must never imply a capability that was not actually verified.

## Assets and trust boundaries

Protected assets include camera firmware and calibration, device availability, video and audio content, configuration and presets, stable identifiers, diagnostic traces, recording destinations, and future daemon credentials.

Trust boundaries are:

1. USB descriptors, kernel-returned structures, and device control payloads entering the process.
2. User and system configuration, device profiles, presets, and research fixtures entering typed parsers.
3. A future local client crossing a Unix-socket boundary into `linkd`.
4. Media frames crossing capture, processing, encoder, file, virtual-camera, and network boundaries.
5. Optional native multimedia libraries, model runtimes, and a vendor SDK crossing into Rust code.
6. Dependencies, development tooling, and CI actions entering the software supply chain.

The physical camera, USB bus, kernel driver, local configuration, third-party profiles, native libraries, and future network clients are not assumed trustworthy merely because they are local.

## Threats and required mitigations

| Threat | Required mitigation |
|---|---|
| Malformed descriptor, profile, config, or payload causes memory corruption or a crash | Strict length checks, bounded allocation, typed parsing, fuzz/property tests, and isolated unsafe ABI code |
| A placeholder, mismatched, or unverified profile authorizes a write | No writable authorization type until full schema, descriptor, firmware, trust, and safety validation exists |
| Raw or rapid XU writes hang or re-enumerate the camera | Raw writes absent from normal builds, central authorization, exact lengths, conservative rate limits, and finite retries |
| Driver detach, reset, firmware, flash, or calibration access damages availability | Deny by default; require separately reviewed, narrowly scoped workflows where the product allows them |
| A non-gimbal device receives mechanical commands | Omit semantic movement commands, deny raw pan/tilt writes even during dry-run, and continuously test both policies |
| A malformed or stale standard-control request changes an unintended value | Resolve against fresh extended-control metadata, validate type/range/step/menu and writability, read before writing, verify readback, and attempt bounded rollback |
| A multi-device mutation changes cameras unintentionally | Require an explicit `--device all --yes` combination and report each device independently |
| Machine output or logs leak serials, paths, credentials, or media | Redact by default, keep logs on stderr, never log secrets or frames, and test diagnostic redaction |
| A local process impersonates or controls the daemon | User-owned socket permissions, peer-credential checks, and protocol-version negotiation before requests |
| Untrusted pipeline text or automation executes commands | Build pipelines and automation from typed structures; arbitrary strings and external processes require explicit local trust |
| A native media library, model runtime, or SDK crashes or corrupts memory | Feature gating, minimal bindings, version checks, and process isolation for the vendor SDK |
| A recording or firmware destination follows a malicious symlink | Canonicalization, regular-file checks, bounded destinations, atomic creation where possible, and explicit sync semantics |
| A network facade exposes camera control | No listener by default; loopback-only defaults, authentication, origin/CSRF controls, and explicit opt-in |
| A dependency or CI action is compromised | Locked Rust dependencies, `cargo deny`, read-only CI permissions, pinned action major versions, and reviewed upgrades |

## Current enforcement

The compiled safety policy admits read-only operations and validated standard V4L2 control writes. It denies raw or unknown XU writes, profile-based vendor writes, driver detach, USB reset, firmware/flash/boot writes, calibration writes, and motor operations. Pan and tilt IDs remain readable inventory but are denied by the write backend. Configuration cannot enable a backend that is not compiled.

Linux discovery and control backends perform bounded device I/O through kernel UVC/V4L2 interfaces. The locally isolated ABI code uses kernel-sized structures and turns typed `errno` failures into stable errors. Mutations read previous values, prefer related-control transactions, verify readback, and report rollback outcomes. Discovery, watches, probes, and `doctor` remain read-only.

There is no daemon listener, media pipeline, network listener, native SDK loading, raw XU write path, or profile write loader. Those boundaries require focused threat-model updates before implementation.

## Review triggers

Update this document before adding a new device-write class, writable profile format, daemon IPC, arbitrary file destination, native library, model download, external process execution, network listener, firmware workflow, or diagnostic bundle containing device data.
