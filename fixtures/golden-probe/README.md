# Link 2C Pro probe fixtures

These directories are read-only, serial-redacted hardware inventories captured from the same Insta360 Link 2C Pro in its normal landscape, native portrait, and U-Disk personalities. Each bundle is validated by `link-testkit`; it contains a normalized JSON report, the kernel-provided USB descriptor blob, and a checksum/redaction manifest.

Regenerate a fixture only from the intended physical mode and always omit `--include-serial`:

```sh
cargo run -p link-cli --features pipewire -- \
  --device usb:1-1 device probe --bundle fixtures/golden-probe/landscape
```
