# Device permissions

`linkctl` should run as the logged-in desktop user. The example rule at `packaging/udev/70-linkctl.rules` applies only to V4L2 nodes beneath the verified Insta360 Link 2C Pro USB identity `2e1a:4c05`. It grants the normal `video` group mode and asks logind to add an ACL for the active local session.

Install and activate the rule as root:

```sh
sudo install -D -m 0644 packaging/udev/70-linkctl.rules \
  /etc/udev/rules.d/70-linkctl.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=video4linux
```

Then verify without elevation:

```sh
linkctl device list
linkctl --device link2cpro-… doctor
linkctl --device link2cpro-… control get brightness
```

If access remains denied, confirm that the graphical session is active, inspect `getfacl /dev/videoN`, and check membership in the distribution's `video` group. Log out and back in after adding group membership. Avoid running `linkctl` with `sudo`: doing so changes configuration paths and can hide a missing user-session ACL.

The rule intentionally does not grant access to unrelated Insta360 products, generic webcams, USB devices, firmware interfaces, storage nodes, or raw USB endpoints. Audio access remains governed by the distribution's normal PipeWire/ALSA session policy.
