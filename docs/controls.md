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

`image status` reports every implemented semantic capability, the supplying backend, evidence, raw descriptor, and current value. A command is available only when an unambiguous standard control is currently enumerated. Unsupported controls fail with exit code 4 and capability evidence rather than guessing a vendor mapping.

```sh
linkctl --device link2cpro-… image status
linkctl --device link2cpro-… image exposure auto
linkctl --device link2cpro-… image exposure manual --shutter 1/120
linkctl --device link2cpro-… image white-balance 5000K
linkctl --device link2cpro-… image focus manual 0.5
linkctl --device link2cpro-… image brightness 0.55
linkctl --device link2cpro-… image anti-flicker 50hz
linkctl --device link2cpro-… image reset
```

Manual shutter, ISO, white balance, focus, and gain first switch an advertised automatic parent to its manual state. `control set --raw` skips those prerequisite changes but still enforces type, range, step, menu, writability, and the movement-control deny policy.

## Mutation guarantees

Use global `--dry-run` to resolve devices, capabilities, prerequisites, values, and intended writes without opening a node for writing. Successful writes include the previous, requested, and observed values in machine output. Readback mismatch or a later failed write triggers best-effort rollback of already changed readable controls.

All-device reads use `--device all`. Mutating `--device all` additionally requires `--yes` and returns per-device results. The direct CLI supports only the standard backend; forcing `vendor` or `host` returns exit code 4, and requiring the unavailable daemon returns exit code 12.

This camera has no mechanical gimbal. Pan and tilt controls may be listed and read for diagnosis, but every attempted write is denied with exit code 9, including raw and dry-run requests. No semantic movement commands are provided.
