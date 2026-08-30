# Safe UVC Extension Unit research

This procedure is subject to the [legal and clean-room notice](legal.md). Use only hardware and software you lawfully
possess or are authorized to test, record the exact controller version and terms presented for it, and keep raw
captures private. Repository contributions must contain only reviewed, minimal, sanitized observations or synthetic
fixtures.

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

Controls declare a typed codec, byte order, exact payload length, read-modify-write behavior, tail policy, snapshot policy, stream requirement and optional exact stream format and warm-up delay, write prelude, verification method and delay, persistence, rollback, minimum write interval, and safety class. Shared selector fields use an explicit positive write mask: decoding considers only those bits, while encoding merges the requested value into a freshly read baseline and preserves every bit outside the mask. A required stream format records its FourCC, dimensions, and rational frame rate so the transfer runs under the same negotiation observed in the source trace. A bounded warm-up delay can preserve an observed interval between stream commit and the first vendor write. The default write prelude checks advertised capabilities; a reviewed trace can instead be reproduced with two consecutive length queries immediately before `SET_CUR`. Verification and warm-up delays are bounded to 60 seconds and must come from observed device timing rather than retries. Bytes outside a typed field are never guessed: a profile must preserve a read baseline, zero them, supply a fixed trailing sequence or full captured write template, or select one of the small deterministic checksum algorithms. A full template is necessary when a writable field appears between required constants and volatile read-only status bytes cannot safely be echoed into `SET_CUR`. The synthetic 52-byte and 61-byte fixtures under `fixtures/xu-profiles/` demonstrate that these rules are profile-specific.

An external directory can be loaded with `--profile-dir`. External profiles remain research input even if their document says `verified`; they cannot authorize `xu set`. Semantic writes require a matching, compiled-in, reviewed `verified` profile. The generic Link 2C Pro bootstrap profile remains read-only and decodes the hardware-verified firmware field for the landscape, Low resolution, and native portrait descriptors. A separate exact-firmware profile verifies its listed camera-native controls—including Auto Framing, image settings, audio pickup, regular Whiteboard, DeskView, gesture switches, 360p compatibility, and native portrait resolution—under their captured stream, timing, transfer, and restart preconditions. Unknown firmware continues to match only the generic bootstrap and cannot authorize the templates.

Writable profile trust is intentionally a build-time decision, not an installed-file signature policy. A proposed
writable mapping must enter the source tree with reviewed evidence and tests; compilation then embeds its exact bytes.
Packages install `profiles.sha256`, while `release-manifest.json` binds the same source profile hashes to the source
revision and attested release artifacts. Replacing or adding a TOML file after installation cannot expand write
authority. There is no runtime trust-store enrollment path for third-party writable profiles.

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

### Windows controller capture

Use Wireshark with USBPcap on the Windows machine that runs the official Insta360 controller. Raw captures belong under the repository-local ignored `traces/` directory (or another private location), never under `fixtures/`.

1. Record the controller version and source, the applicable licence/terms, camera-reported firmware version, selected
   video mode, whether preview is running, and the one setting to be changed. Close other camera applications.
2. Start Wireshark as an administrator and select the `USBPcapN` interface containing USB device `2e1a:4c05`. USBPcap's device list identifies which root hub contains the camera. Do not capture unrelated root hubs.
3. Begin capture before opening the controller. Wait five seconds without interaction to establish a control run.
4. Change exactly one setting from A to B, wait five seconds, restore B to A, and wait another five seconds. Repeat this transition at least three times without changing video mode or stream state.
5. Stop the capture before changing another setting. Save it as `traces/<random-experiment-id>/usb-control-transfers.pcapng` and complete the metadata/notes files described above.
6. Make a separate capture for closed-stream and open-preview conditions, and a separate no-change capture that opens and closes the same controller panel without changing a value.

In Wireshark, first identify the camera's bus/device address from its device descriptor, then use that address with `usb.transfer_type == 0x02` to inspect control transfers. UVC class-interface candidates normally show `SET_CUR` (`bRequest` 0x01) or `GET_CUR` (0x81); `wValue` carries the selector in its high byte and `wIndex` carries the entity/unit and interface. Treat those fields as navigation aids only: the profile must still resolve the entity GUID from the captured descriptor topology and confirm the selector's live `GET_INFO` and `GET_LEN` on Linux.

Before sharing a capture, create a filtered copy containing only the camera address and control transfers, then inspect packet bytes and metadata manually. USB addresses are session-local, so do not reuse a display filter from another capture. The devenv provides `tshark` for offline inspection; for example, list candidate setup packets without altering the source capture:

```sh
devenv shell -- tshark -r traces/<experiment>/usb-control-transfers.pcapng \
  -Y 'usb.transfer_type == 0x02 && usb.setup.bRequest' \
  -T fields -e frame.number -e usb.bus_id -e usb.device_address \
  -e usb.bmRequestType -e usb.setup.bRequest \
  -e usb.setup.wValue -e usb.setup.wIndex -e usb.setup.wLength
```

Do not redact by editing the only capture. Keep the original private, generate a derived file, and verify the derived packet set before it is considered for a reviewed fixture.

## Recovery and diagnostics

`linkctl --device … xu recover` only closes/reopens control handles, compares a fresh safe inventory, and rebuilds a temporary no-output GStreamer pipeline when that backend is present. It does not detach the kernel driver or reset the USB device. If that bounded recovery fails, stop issuing writes and physically reconnect the camera rather than retrying rapidly.

`linkctl doctor --bundle report.tar.zst` writes a new owner-only, no-clobber archive containing the diagnostic report, host and runtime metadata, profile checksums, redacted per-device probes, the payload-free XU audit log when it is small and regular, per-file SHA-256 values, and an explicit omitted/redacted list. Kernel logs are intentionally omitted because they can contain unrelated host identifiers.
