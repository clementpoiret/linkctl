# Firmware maintenance

`linkctl` supports Insta360's manual U-Disk update path. It does not download firmware, synthesize touch input, send a
bootloader command, write a firmware USB control, unmount storage, or disconnect the camera. Obtain the file and any
published checksum from [Insta360's official firmware instructions](https://onlinemanual.insta360.com/link2cpro/en-us/tutorial/maintenance%2Bcare/firmware-update),
and review the [official U-Disk entry procedure](https://onlinemanual.insta360.com/link2cpro/en-us/faq/operation-guide/usb-mode)
before starting.

`linkctl` does not grant a right to obtain or use firmware. Supply only a file obtained from an authorized source and
review the project's [legal and clean-room notice](legal.md). No firmware file is included in source or release
artifacts.

## Observe the maintenance transition

Use JSON Lines for automation or the default human output for an operator:

```sh
linkctl --device usb:10-3 firmware watch
linkctl --format jsonl --device usb:10-3 firmware watch
```

The watch follows one physical USB topology across normal camera and U-Disk re-enumeration. It reports add, remove,
mode-change, and volume-mount changes. The accepted U-Disk personality is guarded by the recorded USB identity,
revision, and descriptor fingerprint. Its only accepted staging volume is a `vfat` filesystem labelled
`LINK_2C_PRO`. Mount paths are used internally and are not included in watch or operation reports.

Normal camera operations are rejected while the selected device is in U-Disk mode. Read-only device inspection and
firmware maintenance commands remain available.

## Validate and stage an official file

The filename must be exactly `Insta360LINK2CPROFW_HOST.bin`. A dry run hashes the source and validates every check that
is possible in the current USB mode without copying data or creating an operation log:

```sh
linkctl --device usb:10-3 --dry-run firmware stage ./Insta360LINK2CPROFW_HOST.bin
linkctl --device usb:10-3 --dry-run firmware stage ./Insta360LINK2CPROFW_HOST.bin \
  --sha256 <64-hex-digit-official-checksum>
```

When the camera is still in normal mode, the dry run reports that volume, mount, free-space, and destination checks
cannot run until the maintenance volume appears. Running the real workflow requires explicit confirmation:

```sh
linkctl --device usb:10-3 --yes firmware stage ./Insta360LINK2CPROFW_HOST.bin \
  --sha256 <64-hex-digit-official-checksum>
```

The command performs the following bounded workflow:

1. It rejects symbolic links, non-regular files, wrong filenames, files outside the 16–256 MiB bounds, and a checksum
   mismatch.
2. If necessary, it asks the operator to triple-tap the touch key and hold it for five seconds. It only continues after
   the exact U-Disk personality appears at the selected topology.
3. It waits for the exact labelled `vfat` volume, then rejects an existing destination, abandoned linkctl temporary
   files, and insufficient free space.
4. It copies in chunks to a no-clobber temporary file, reports progress, re-hashes the source, synchronizes the file,
   renames it without replacement, synchronizes the volume directory, and verifies the staged hash.
5. Only after synchronization does it ask the operator to disconnect and reconnect the camera. It waits for normal
   mode at the same topology and compares the pre/post firmware versions when readable.

Each manual transition has a five-minute timeout by default. Use `--transition-timeout`, for example `10m`, when the
host needs longer to mount or re-enumerate the device. `--format json` emits one final report; `--format jsonl` emits
progress events followed by the final report.

## Logs and interruption

Real staging creates an owner-only JSON operation log under `$XDG_STATE_HOME/linkctl/firmware/`, falling back to
`~/.local/state/linkctl/firmware/`. It contains the source hash, matched volume identity, progress history,
synchronization state, version observations, errors, and recovery guidance. It never contains firmware bytes or a
mount path.

Before synchronization, an interruption removes linkctl's temporary destination when possible and reports that the
copy was not completed. After synchronization, the file may already be actionable by the camera: follow the reported
guidance and do not repeat or remove it casually. `linkctl` never claims completion until the copy and directory are
synchronized; failure to observe the camera returning is reported as partial success with the operation-log path.
