# Hardware probe

`linkctl device list` groups USB devices with their V4L2, media-controller, ALSA, and mass-storage nodes. It prints non-secret stable IDs and redacts USB serials by default. Select a device for a full inventory by stable ID, serial, `usb:<topology>`, `/dev/video*`, or an unambiguous `/dev/*/by-id` or `/dev/*/by-path` alias.

```sh
linkctl device list
linkctl --format json --device all device probe
linkctl --device usb:1-1 device probe
```

Read-only `device info`, `caps controls`, and `device probe` accept `--device all`. A reusable bundle contains one physical device, so `device probe --bundle` requires an exact selector.

The probe reads USB descriptors, classifies every associated video node, enumerates current/advertised V4L2 formats and standard controls, parses UVC Extension Units, asks advertised selectors only for `GET_LEN` and `GET_INFO`, and correlates ALSA/PipeWire audio sources. The probe does not set controls, negotiate formats, start streams, detach drivers, reset USB devices, or read XU payloads.

For focused XU analysis, `xu inventory` exposes the complete retained VideoControl descriptor graph and `xu get` performs an exact-length selector read. The snapshot/diff workflow and sanitization rules are documented in [Safe UVC Extension Unit research](xu-research.md).

## Reusable bundles

Pass `--bundle` with a path that does not exist:

```sh
linkctl --device /dev/video2 device probe --bundle fixtures/golden-probe/landscape
```

The new directory contains:

- `probe.json`: the normalized report;
- `usb-descriptors.bin`: the kernel-provided binary device/configuration descriptors (USB string values are not expanded into this blob);
- `manifest.json`: SHA-256 checksums and the redaction policy.

Existing destinations are rejected. If bundle creation fails, the partially created directory is removed. Normal bundles omit serials, usernames, home and mount paths, credentials, logs, and media frames. `--include-serial` is deliberately explicit and marks the manifest accordingly.

The report records USB `bcdDevice` separately from firmware and never treats that revision as a firmware version. For the verified landscape, Low resolution, and portrait descriptors, `firmware info` reads a bounded UTF-8 field from XU selector 3 and currently reports `v0.2.9.8_build3`. Profile selection first uses an exact descriptor/device bootstrap profile, decodes `firmware.version` when that profile supplies it, and then performs a second match in which an exact firmware guard outranks the bootstrap profile. Unknown firmware therefore retains standard and read-only discovery capabilities without authorizing vendor writes.

`firmware watch` follows the camera's physical topology across the normal and U-Disk identities and reports the
validated volume without disclosing its mount path. `firmware stage` uses the same exact profile guard plus the
recorded `LINK_2C_PRO`/`vfat` volume identity before it can copy the fixed official filename. See
[Firmware maintenance](firmware.md) for the operator workflow and failure semantics.

Build with the `pipewire` feature to include native PipeWire registry correlation. ALSA discovery remains available in every build.

## Recorded hardware evidence

The checked-in golden bundles capture three personalities from the same Link 2C Pro:

- Landscape camera mode enumerates as `2e1a:4c05`, revision `0200`, with one capture node, one metadata node, MJPEG/H.264 formats, 17 V4L2 controls, three UVC Extension Units, mono 48 kHz S16_LE audio, and associated PipeWire objects.
- Native portrait mode keeps the camera USB identity but changes the descriptor fingerprint and advertises portrait sizes including 1088x1920 and 2176x3840.
- U-Disk mode enumerates as `070a:4026`, revision `0001`, with a FAT volume labelled `LINK_2C_PRO`; it has no V4L2, XU, or audio nodes. The fixture was captured while the volume was unmounted.

All three descriptor fingerprints are exact guards in the read-only profile. A different descriptor fingerprint does not match that profile.
