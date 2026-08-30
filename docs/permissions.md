# Device permissions

`linkctl` should run as the logged-in desktop user. The example rule at `packaging/udev/70-linkctl.rules` applies only to V4L2 nodes beneath the verified Insta360 Link 2C Pro USB identity `2e1a:4c05`. It grants the normal `video` group mode and asks logind to add an ACL for the active local session.

Native Debian, Fedora, and Arch packages install the rule under `/usr/lib/udev/rules.d`. For an unpackaged source build
on those distributions, install and activate it as root:

```sh
sudo install -D -m 0644 packaging/udev/70-linkctl.rules \
  /etc/udev/rules.d/70-linkctl.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=video4linux
```

On NixOS, do not copy the rule imperatively into `/etc`. Register the Nix package that contains the rule in the system
configuration instead:

```nix
{
  services.udev.packages = [ linkctlPackage ];
}
```

The [NixOS installation instructions](user-guide.md#nixos) show how to define `linkctlPackage`, expose the binaries,
and register the systemd user unit. After `nixos-rebuild switch`, reconnect the camera or run
`sudo udevadm trigger --subsystem-match=video4linux` to apply the rule to existing video nodes. NixOS restarts udev
when the declared rule set changes, so a separate `udevadm control --reload-rules` is normally unnecessary.

Then verify without elevation:

```sh
linkctl device list
linkctl --device link2cpro-… doctor
linkctl --device link2cpro-… control get brightness
```

If access remains denied, confirm that the graphical session is active, inspect `getfacl /dev/videoN`, and check membership in the distribution's `video` group. Log out and back in after adding group membership. Avoid running `linkctl` with `sudo`: doing so changes configuration paths and can hide a missing user-session ACL.

The rule intentionally does not grant access to unrelated Insta360 products, generic webcams, USB devices, firmware interfaces, storage nodes, or raw USB endpoints. Firmware staging relies on the desktop session's ordinary removable-media mount and access policy; `linkctl` does not broaden block-device permissions or mount the U-Disk itself. Audio access remains governed by the distribution's normal PipeWire/ALSA session policy. Run PipeWire commands in the logged-in user's session so the client can reach that user's PipeWire socket and routing metadata. On ALSA-only systems, the user may also need membership in the distribution's `audio` group or an equivalent logind ACL. Verify access without elevation:

```sh
linkctl audio devices
linkctl --device link2cpro-… audio status
linkctl --device link2cpro-… audio meter --duration 1s
```

An exclusive direct ALSA capture reports the endpoint as busy. Prefer the PipeWire route on desktop systems when other applications must share the microphone. Avoid `sudo`: root normally cannot use the desktop user's PipeWire session and would create root-owned recording files.
