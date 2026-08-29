# Safe UVC Extension Unit research

`linkctl` can inventory and read UVC Extension Units without guessing selector lengths or semantics. The normal workflow is read-only:

```sh
linkctl --device /dev/video2 xu inventory
linkctl --device /dev/video2 xu get \
  --guid faf1672d-b71b-4793-8c91-7b1c9b7f95f8 --selector 1
linkctl --device /dev/video2 xu snapshot baseline.json --samples 5 \
  --note "controller closed; stream closed"

# Change exactly one setting with the official controller, then capture again.
linkctl --device /dev/video2 xu snapshot after.json --samples 5 \
  --note "changed exactly one setting"
linkctl xu diff baseline.json after.json
```

`inventory` parses the complete VideoControl entity graph and reports each Extension Unit's canonical GUID, runtime unit ID, source pins, control bitmap, advertised selectors, descriptor location, and safe `GET_INFO`/`GET_LEN` results. `get` resolves a GUID or explicit unit, proves that the selector is advertised, obtains fresh capability and length data, allocates exactly that length, and issues `GET_CUR` only when GET is supported. It prints raw hex and base64 plus any typed decode supplied by an exact matching profile.

Snapshots contain repeated raw samples, a per-bit volatility mask, standard V4L2 controls, stream notes, redacted device identity, descriptor fingerprint, and matched-profile checksum. Selectors marked `volatile` or `exclude` by a profile are left out unless explicitly requested. `xu diff` is hardware-free and aligns selectors by GUID rather than runtime unit ID; `xu watch --format jsonl` emits the same byte, bit, and typed-value differences continuously.

## Profiles and trust

Profiles are strict versioned TOML. The data model is documented by [vendor-profile-v1.json](schemas/vendor-profile-v1.json), while snapshots use [xu-snapshot-v1.json](schemas/xu-snapshot-v1.json). A match includes USB mode, VID/PID, a `bcdDevice` range, exact descriptor SHA-256, and—when writes are described—an exact firmware allow-list. Unknown fields, placeholder fingerprints, invalid GUIDs, overlapping payload fields, inconsistent lengths, and writable controls without provenance and trace IDs are rejected.

Controls declare a typed codec, byte order, exact payload length, read-modify-write behavior, tail policy, snapshot policy, stream requirement, verification method, persistence, rollback, minimum write interval, and safety class. Tail bytes are never guessed: a profile must preserve a read baseline, zero the tail, supply fixed bytes, or select one of the small deterministic checksum algorithms. The synthetic [52-byte and 61-byte fixtures](../fixtures/xu-profiles/README.md) demonstrate that these rules are profile-specific.

An external directory can be loaded with `--profile-dir`. External profiles remain research input even if their document says `verified`; they cannot authorize `xu set`. Semantic writes require a matching, compiled-in, reviewed `verified` profile. The checked-in Link 2C Pro profile is deliberately read-only because no vendor mapping has yet met that standard.

## Experimental write boundary

The `xu raw-set` command is present so scripts and help remain stable, but its transport is absent from normal builds. An operational raw write requires all of the following:

- a binary built with the non-default `research` Cargo feature;
- `safety.allow_raw_xu = true` in effective configuration;
- the explicit `--unsafe-xu` acknowledgement;
- an exact experimental or verified profile match, including known firmware for writable controls;
- a writable profile control at the same GUID, selector, and `GET_LEN` payload length;
- the normal safety class (firmware, boot, flash, calibration, and motor classes are always denied);
- no conflicting linkctl device/media lease;
- the profile's stream-state requirement and conservative write interval.

The command re-queries `GET_INFO` and `GET_LEN` immediately before `SET_CUR`, never writes an unknown selector, and appends an owner-only audit record containing payload length and SHA-256 rather than payload bytes. `--dry-run` performs the same authorization and profile checks without issuing SET or advancing the rate limiter.

`xu set` is narrower: it accepts a semantic name and typed value only from a trusted compiled-in verified profile, constructs the exact payload according to its tail rule, and currently requires direct readback verification. A profile can require a temporary no-output stream during the operation or a pipeline rebuild afterward. No path detaches `uvcvideo`, performs a USB reset, or enters firmware/boot modes.

## Capture and sanitization

Keep raw experiments outside the repository until they have been reviewed. A useful private trace directory contains:

```text
metadata.json
before.json
after.json
usb-control-transfers.pcapng
notes.md
```

`metadata.json` should record a random experiment ID, firmware source/value, descriptor SHA-256, video tuple, stream state, application version, changed setting, repetition number, and verification observation. Before publishing, remove USB serials, hostnames, usernames, home/mount paths, unrelated USB traffic, audio/video content, credentials, and unique controller/account identifiers. Prefer replacing identifiers with consistent synthetic tokens so correlations remain testable. Never publish a pcapng merely because the JSON snapshots are clean.

Repository fixtures contain only normalized JSON or synthetic bytes. Each fixture README must state its origin, sanitization, expected parser behavior, and whether any byte came from hardware. Raw captured payloads require explicit review.

## Recovery and diagnostics

`linkctl --device … xu recover` only closes/reopens control handles, compares a fresh safe inventory, and rebuilds a temporary no-output GStreamer pipeline when that backend is present. It does not detach the kernel driver or reset the USB device. If that bounded recovery fails, stop issuing writes and physically reconnect the camera rather than retrying rapidly.

`linkctl doctor --bundle report.tar.zst` writes a new owner-only, no-clobber archive containing the diagnostic report, host and runtime metadata, profile checksums, redacted per-device probes, the payload-free XU audit log when it is small and regular, per-file SHA-256 values, and an explicit omitted/redacted list. Kernel logs are intentionally omitted because they can contain unrelated host identifiers.
