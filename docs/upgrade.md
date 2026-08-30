# Upgrade guide

## Before upgrading

1. Stop active recordings and streams.
2. Save `linkctl doctor --bundle linkctl-before-upgrade.tar.zst` if diagnostics may be needed.
3. Back up `$XDG_CONFIG_HOME/linkctl` and `$XDG_STATE_HOME/linkctl` (or their default locations).
4. Stop the user daemon: `systemctl --user stop linkd.service`.

Pre-release binaries have no compatibility guarantee. The 1.0 release freezes machine-output schema 1, preset schema
1, transaction schema 1, vendor-profile schema 1, XU snapshot schema 1, and daemon protocol 1. No data migration is
required for valid schema-1 files, but strict parsers will continue to reject unknown or malformed fields.

## Package upgrade

Install exactly one distribution package and do not mix its binaries with a source or Nix build. Packages do not enable
or start the daemon automatically.

After replacing the package:

```sh
systemctl --user daemon-reload
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=video4linux
linkctl --version
linkctl doctor
```

The udev reload is an installation action; normal runtime remains unprivileged. If the daemon was enabled before the
upgrade, start it explicitly with `systemctl --user start linkd.service` and check `linkctl daemon status`.

## Behavioral changes at 1.0

- Standard builds contain `daemon`, `gstreamer`, and `pipewire`; `research`, `network`, and `host-ai` are excluded.
- Vendor writes are authorized only by compiled-in verified profiles. An external profile can aid safe inspection but
  cannot grant write authority.
- The daemon protocol and machine-output schemas are version 1. A newer incompatible protocol is rejected rather than
  guessed.
- Source revision appears in probes, doctor output, diagnostic bundles, and daemon status when embedded by the build.

## Rollback

Stop `linkd`, reinstall the prior package from a trusted local artifact, reload the user manager, and run `doctor`.
Do not downgrade a configuration file by deleting fields until the prior binary has first reported why it rejects the
file. Firmware is not rolled back by package installation; the package never stages firmware implicitly.
