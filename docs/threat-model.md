# Threat model

## Security objectives

The system must preserve camera availability, prevent unverified device writes, keep local media and identifiers private, and ensure that only the logged-in user can control future long-running services. A successful command must never imply a capability that was not actually verified.

## Assets and trust boundaries

Protected assets include camera firmware and calibration, device availability, video and audio content, configuration and presets, stable identifiers, diagnostic traces, recording destinations, and future daemon credentials.

Trust boundaries are:

1. USB descriptors, kernel-returned structures, and device control payloads entering the process.
2. User and system configuration, device profiles, presets, and research fixtures entering typed parsers.
3. A local `linkctl` client crossing the private Unix-socket boundary into `linkd`.
4. Media frames crossing capture, processing, encoder, file, virtual-camera, and network boundaries.
5. Optional native multimedia libraries, model runtimes, and a vendor SDK crossing into Rust code.
6. Dependencies, development tooling, and CI actions entering the software supply chain.

The physical camera, USB bus, kernel driver, local configuration, third-party profiles, native libraries, and future network clients are not assumed trustworthy merely because they are local.

## Threats and required mitigations

| Threat | Required mitigation |
|---|---|
| Malformed descriptor, profile, config, or payload causes memory corruption or a crash | Strict length checks, bounded allocation, typed parsing, fuzz/property tests, and isolated unsafe ABI code |
| A placeholder, mismatched, or unverified profile authorizes a write | Strict schema and placeholder rejection, exact descriptor/firmware guards, loader-assigned trust, compiled-in verification for semantic writes, and independent safety classification |
| Raw or rapid XU writes hang or re-enumerate the camera | Raw writes absent from normal builds, central authorization, exact lengths, conservative rate limits, and finite retries |
| Driver detach, reset, firmware, flash, or calibration access damages availability | Deny by default; require separately reviewed, narrowly scoped workflows where the product allows them |
| A non-gimbal device receives mechanical commands | Omit semantic movement commands, deny raw pan/tilt writes even during dry-run, and continuously test both policies |
| A malformed or stale standard-control request changes an unintended value | Resolve against fresh extended-control metadata, validate type/range/step/menu and writability, read before writing, verify readback, and attempt bounded rollback |
| A malformed or model-incompatible preset partially changes a camera | Strict versioned parsing, exact model/optional USB guards, complete preflight, per-device leases, dependency ordering, verification, reverse rollback, and a crash-visible recovery journal |
| A preset leaks stream credentials | The current preset schema has no recording/stream target or inline credential field; unknown fields fail and future integrations must use secret references |
| A multi-device mutation changes cameras unintentionally | Require an explicit `--device all --yes` combination and report each device independently |
| Machine output or logs leak serials, paths, credentials, microphone levels, or media | Redact identifiers by default, keep logs on stderr, never log secrets or media buffers, and emit only explicitly requested aggregate audio levels |
| A local process impersonates or controls the daemon | Owner-only runtime directory and socket, same-UID peer-credential checks on client and server, bounded frames, strict JSON decoding, and protocol-version negotiation before dispatch |
| Untrusted pipeline text or automation executes commands | Build pipelines and automation from typed structures; arbitrary strings and external processes require explicit local trust |
| A native media library, model runtime, or SDK crashes or corrupts memory | Feature gating, minimal bindings, version checks, and process isolation for the vendor SDK |
| A recording or audio-capture destination follows a malicious symlink | Canonicalization, regular-file checks, bounded destinations, same-directory temporary output, atomic finalization where possible, and explicit sync semantics |
| A spoofed source or unrelated mounted volume receives firmware | Require the exact official filename, regular-file/no-symlink input, bounded size and optional trusted checksum, exact USB revision/descriptor/profile/topology matching, and the exact filesystem label and type |
| A firmware copy is replaced, truncated, duplicated, or interrupted | Reject existing and abandoned destinations, use no-follow/create-new temporary output and no-replace rename, hash while copying, sync the file and directory, verify the final hash, retain a private operation log, and distinguish pre-sync failure from post-sync partial success |
| A network facade exposes camera control | No listener by default; loopback-only defaults, authentication, origin/CSRF controls, and explicit opt-in |
| A dependency or CI action is compromised | Locked Rust dependencies, `cargo deny`, read-only CI permissions, pinned action major versions, and reviewed upgrades |

## Current enforcement

The compiled safety policy admits read-only operations, validated standard V4L2 writes, and semantic XU writes only for compiled-in trusted verified profiles. Unknown XU writes, driver detach, USB reset, firmware/flash/boot device-control writes, calibration writes, and motor operations are denied. Firmware staging is a separate filesystem-only workflow: it accepts an explicit local file, follows the official manual U-Disk transition, and cannot download firmware, synthesize touch input, mount or unmount storage, disconnect USB, or send a firmware control. The separately compiled research transport additionally requires explicit acknowledgement, configuration opt-in, an exact experimental/verified profile and payload classification, fresh device-reported length/capabilities, conservative pacing, and device/media leases. Pan and tilt IDs remain readable inventory but are denied by the standard write backend. Configuration cannot enable a backend that is not compiled.

Linux discovery and control backends perform bounded device I/O through kernel UVC/V4L2 interfaces. The locally isolated XU ABI code asserts the kernel structure layout, keeps one file descriptor open per transaction, obtains `GET_INFO` and little-endian `GET_LEN` before reads, validates the length immediately before writes using the matched profile's reviewed prelude, and turns typed `errno` failures into stable errors. Mutations read previous values, prefer related-control transactions, verify readback, and report rollback outcomes. Discovery, read watches, probes, snapshots, diffs, and `doctor` remain device-read-only; explicitly requested artifacts use no-clobber semantics and bounded destinations.

Firmware maintenance follows the same physical camera by USB topology while it changes personality. The accepted
U-Disk identity is constrained by the built-in read-only profile, and the associated block volume must have the
recorded label and filesystem. Staging takes the device and media leases, validates and hashes the source before the
power/disconnect warning, copies only to the fixed official destination name, synchronizes before reporting success,
and records every state transition in an owner-only log. Normal camera commands are centrally rejected in U-Disk
mode. The path still relies on the user's mounted filesystem and kernel storage stack, which are untrusted inputs.

Preset and direct media/control operations share a user-owned cross-process lease. Preset files use atomic no-clobber writes with owner-only directories and files. An apply journal is updated after each verified or rolled-back stage and removed only after success or complete restoration; incomplete journals block another apply and are reported by `doctor`.

Typed GStreamer pipelines now accept video and audio buffers from selected kernel/PipeWire endpoints and write only explicit recording, snapshot, audio-capture, standard-output, playback, virtual-camera, or RTP destinations. Pipeline text is never accepted from configuration or the command line. Virtual sinks must be writable V4L2 output nodes, and daemon recordings reject symbolic-link destinations. Binary standard output contains only media bytes; diagnostics use stderr. Direct single-file recordings and audio captures use same-directory temporary files and are renamed only after clean finalization. Monitoring selects an existing playback route and does not create a network listener.

The daemon listens only on its local Unix socket and exposes no general network listener. It serializes graph changes through one bounded actor and limits JSON and binary frame sizes. There is no native SDK loading. External profiles are research-only input and cannot authorize semantic writes. Diagnostic archives are owner-only, no-clobber, checksummed, serial-redacted, and enumerate omitted fields; raw payloads are never added to their audit log.

## Review triggers

Update this document before adding or changing a device-write class, writable profile format, daemon IPC, arbitrary file destination, native library, model download, external process execution, network listener, firmware workflow, or diagnostic bundle containing device data.
