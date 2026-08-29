# Standard camera controls

`linkctl` enumerates the controls advertised by the selected V4L2 capture node at runtime. Names are stable snake-case forms of kernel names; a decimal ID, hexadecimal ID, or exact kernel name can also select a control.

```sh
linkctl --device link2cpro-… control list
linkctl --device link2cpro-… control get brightness
linkctl --device link2cpro-… control set brightness 55%
linkctl --device link2cpro-… control reset brightness
linkctl --device link2cpro-… control watch brightness --format jsonl
```

Boolean controls accept values such as `on`, `off`, `true`, and `false`. Menu controls accept an advertised label or numeric index. Integer controls accept raw integers; controls with an unambiguous range also accept normalized percentages. Values outside the reported range or step are rejected unless a normalized image command explicitly uses `--clamp`.

Submit an ordered related-control transaction with repeated batch values:

```sh
linkctl --device link2cpro-… control set \
  --batch brightness=55% contrast=50%
```

The command uses `VIDIOC_S_EXT_CTRLS`. If the driver rejects the batch, the error reports its `error_idx`. `--fallback-individual` permits an ordered fallback only after previous values have been captured for rollback.

## Semantic image controls

`image status` reports every implemented semantic capability, the supplying backend, evidence, raw descriptor, and current value. Standard controls are preferred when present; an exact trusted camera profile may supply a missing semantic control. Unsupported controls fail with exit code 4 and capability evidence rather than guessing a vendor mapping.

```sh
linkctl --device link2cpro-… image status
linkctl --device link2cpro-… image exposure auto
linkctl --device link2cpro-… image exposure manual --shutter 1/120
linkctl --device link2cpro-… image white-balance auto
linkctl --device link2cpro-… image white-balance 5000K
linkctl --device link2cpro-… image focus manual 0.5
linkctl --device link2cpro-… image brightness 0.55
linkctl --device link2cpro-… image anti-flicker 50hz
linkctl --device link2cpro-… image reset
```

Manual shutter, ISO, white balance, focus, and gain first switch an advertised automatic parent to its manual state. `control set --raw` skips those prerequisite changes but still enforces type, range, step, menu, writability, and the movement-control deny policy.

The Link 2C Pro exposes automatic exposure, ISO, and shutter through its verified camera profile rather than standard V4L2 controls. `image exposure manual` writes manual mode first, followed by the specified ISO and shutter values under one rollback-capable transaction. ISO accepts the Controller's 100–3200 range. Shutter accepts fractions or durations from 1/8000 through 1/30 and encodes the rounded denominator used by the camera. Fractional-rate shutter values can read back one denominator lower, so the shutter profile permits a one-unit numeric difference and rejects anything larger. Unspecified manual fields are preserved. Status reports the actual mode, ISO, and shutter readback together.

On the same camera profile, `image exposure-compensation` accepts -3.0 through +3.0 EV in 0.1 EV steps. Selector 9 stores signed hundredths of an EV, so the semantic command and status convert reversibly between EV and the raw two-byte value. A standard V4L2 exposure-compensation control remains preferred when present.

The same Controller capture contains a separate exposure-curve protocol on selector 16. Curve changes use three 255-byte writes and have no observed `GET_CUR`, so they cannot meet the semantic readback and rollback contract and are not exposed.

On the Link 2C Pro, the official controller capture confirms that white balance uses the standard UVC Processing Unit controls exposed by V4L2, not a vendor Extension Unit. Automatic mode is `white_balance_automatic`; manual temperature is `white_balance_temperature`. The controller writes automatic mode off before setting a manual temperature. Its UI exercised 2000 K, 4800 K, and 10000 K, with 4800 K as its default selection. Linux target-hardware tests verified the same endpoints and three complete manual/automatic cycles, with 1 K steps reported by the live descriptor. `image status` reports the live mode and Kelvin value together, while the capability record derives its accepted range and step from that descriptor.

Focus uses the standard UVC Camera Terminal controls `focus_automatic_continuous` and `focus_absolute`. The Controller capture confirms that manual focus disables autofocus before writing a direct 0–100 absolute value. The semantic command exposes this as a normalized 0.0–1.0 position derived from the live descriptor. Status reports autofocus/manual mode and normalized position together; JSON retains the underlying raw control descriptor and value. Linux hardware tests verified three endpoint/autofocus cycles, exact `0.37` conversion, rejection before write, and restoration of the original autofocus state.

Anti-flicker uses the standard UVC Processing Unit `power_line_frequency` control. The Controller capture maps raw `1`, `2`, and `3` to 50 Hz, 60 Hz, and automatic respectively. The target Linux descriptor advertises disabled (`0`), 50 Hz (`1`), and 60 Hz (`2`), while inconsistently reporting automatic (`3`) as an out-of-range default. Semantic status and capability output use stable `disabled`, `50hz`, and `60hz` names for the advertised values. Requests for `auto` fail with exit code 4 before writing because the kernel does not expose that captured mode as writable.

## Digital zoom

The Link 2C Pro exposes standard `zoom_absolute` from 100 through 400 in steps of one. `linkctl` renders those raw units as 1.00x through 4.00x and continues to validate the live descriptor rather than assuming that range for another device:

```sh
linkctl --device link2cpro-… zoom get
linkctl --device link2cpro-… zoom set 1.5x
linkctl --device link2cpro-… zoom step +0.1x
linkctl --device link2cpro-… zoom ramp 1x 2x --duration 750ms
linkctl --device link2cpro-… zoom reset
```

`ramp` uses a bounded 20 Hz sequence, verifies the final value, and restores the starting value if an intermediate or final write fails. Durations below 50 ms or above 60 seconds are rejected. Camera-native frame translation remains a separate capability and is never synthesized by changing pan or tilt controls.

## Camera-native controls

`caps all` combines standard controls with the fixed-mount camera-native capability set. Status commands remain useful before a vendor mapping exists: their machine output identifies the item as `discovered-unmapped` and does not read or write a guessed selector. See the [camera-native capability matrix](camera-native.md) for the exact commands and current evidence.

## Mutation guarantees

Use global `--dry-run` to resolve devices, capabilities, prerequisites, values, and intended writes without opening a node for writing. Successful writes include the previous, requested, and observed values in machine output. Readback mismatch or a later failed write triggers best-effort rollback of already changed readable controls.

All-device reads use `--device all`. Mutating `--device all` additionally requires `--yes` and returns per-device results. Standard image and zoom operations use the standard backend. A vendor operation is admitted only for an exact trusted built-in profile; an unmapped operation returns exit code 4. Requiring the unavailable daemon returns exit code 12.

This camera has no mechanical gimbal. Pan and tilt controls may be listed and read for diagnosis, but every attempted write is denied with exit code 9, including raw and dry-run requests. No semantic movement commands are provided.
