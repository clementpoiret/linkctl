# Nix package omits GStreamer core plugins, and `caps all` aborts on an individually readable capability

## Summary

`linkctl 1.0.0` on NixOS cannot load the GStreamer `capsfilter` element because its generated wrapper includes `gst-plugins-base`, `good`, `bad`, and `libav`, but omits the core GStreamer plugin directory containing `libgstcoreelements.so`.

This causes multiple supported camera operations to return:

```text
capability-unsupported: required GStreamer element is unavailable
```

After supplying the matching GStreamer core plugin directory manually, those operations succeed.

There is also a separate `caps all` issue: it aborts the entire capability report because `mode.compatibility` allegedly requires an incompatible stream state, even though querying `mode compatibility status` directly succeeds.

## Environment

```text
OS: NixOS 26.11
linkctl: 1.0.0
linkctl source revision: 93c816fc93bed6695d190c982f68ea1beccf3cdb
Camera: Insta360 Link 2C Pro
Camera state: ready
Camera profile: insta360-link-2c-pro
```

All reproduction commands below are read-only.

## Issue 1: Missing GStreamer core plugin path

### Reproduction

```bash
common=(
  --daemon never
  --log-level off
  --format json
  --schema-version 1
  --no-color
  --timeout 15s
)

device_json=$(linkctl "${common[@]}" device list)
device=$(jq -r '.result[0].stable_id' <<<"$device_json")

linkctl --device "$device" "${common[@]}" image status
```

### Actual result

The process exits with code 4:

```json
{
  "schema_version": 1,
  "ok": false,
  "command": "image.status",
  "result": null,
  "error": {
    "code": "capability-unsupported",
    "exit_code": 4,
    "message": "required GStreamer element is unavailable"
  }
}
```

The same failure occurs for:

```text
auto-framing status
mode whiteboard status
mode deskview status
gesture status
```

### Package inspection

The `linkctl` derivation wraps the binaries with this plugin path:

```nix
pluginPath =
  "${gst-plugins-base}/lib/gstreamer-1.0:"
  + "${gst-plugins-good}/lib/gstreamer-1.0:"
  + "${gst-plugins-bad}/lib/gstreamer-1.0:"
  + "${gst-libav}/lib/gstreamer-1.0";

wrapProgram "$out/bin/linkctl" \
  --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "$pluginPath"

wrapProgram "$out/bin/linkd" \
  --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "$pluginPath"
```

It does not include:

```nix
${gstreamer}/lib/gstreamer-1.0
```

That directory contains `libgstcoreelements.so`, which provides `capsfilter`.

The executable is built against GStreamer 1.26.11, while the launching desktop environment exports GStreamer 1.28.6 plugin directories. A GStreamer 1.26 `gst-inspect-1.0` therefore behaves as follows:

```text
Current desktop plugin environment:
gst-inspect-1.0 capsfilter -> exit 255

With gstreamer-1.26.11/lib/gstreamer-1.0 included:
gst-inspect-1.0 capsfilter -> exit 0
```

### Isolation result

When the matching GStreamer 1.26 core plugin directory is added to `GST_PLUGIN_SYSTEM_PATH_1_0`, all previously failing commands succeed:

```text
image status              exit 0
auto-framing status       exit 0
mode whiteboard status    exit 0
mode deskview status      exit 0
gesture status            exit 0
```

This confirms that the camera capabilities themselves are not the cause of these errors.

### Desired behavior

The Nix wrapper for both `linkctl` and `linkd` should provide a complete, version-consistent GStreamer plugin environment, including:

```nix
${gstreamer}/lib/gstreamer-1.0
```

At minimum, the packaged executable should be able to instantiate every element required by its supported operations, including `capsfilter`, regardless of an incompatible GStreamer plugin path inherited from the desktop session.

`linkctl doctor` should also verify required GStreamer elements. It currently reports `healthy: true` even while normal commands fail because `capsfilter` is unavailable.

## Issue 2: `caps all` aborts on `mode.compatibility`

This remains reproducible even after supplying the correct GStreamer 1.26 core plugin directory.

### Reproduction

```bash
linkctl --device "$device" "${common[@]}" caps all
```

### Actual result

The process exits with code 8 and returns no capability result:

```json
{
  "schema_version": 1,
  "ok": false,
  "command": "caps.all",
  "result": null,
  "error": {
    "code": "protocol-profile-mismatch",
    "exit_code": 8,
    "message": "requested vendor reads require incompatible stream states",
    "details": {
      "control": "mode.compatibility"
    }
  }
}
```

However, the individual query succeeds:

```bash
linkctl --device "$device" "${common[@]}" mode compatibility status
```

```text
Exit code: 0
Command: mode.compatibility
Capability readable: true
Capability writable: true
Current value: standard
```

`linkctl caps controls` also succeeds and returns a usable semantic capability map.

### Desired behavior

According to its help text, `caps all` should report every implemented, unavailable, and unmapped semantic capability. A single capability with incompatible read preconditions should not discard the complete report.

Acceptable behavior would be either:

- Perform the individual reads using compatible stream-state transitions.
- Return the remaining capability map and mark only the affected capability's current value as unavailable or unknown.

Because `mode compatibility status` succeeds independently, `caps all` should ideally use the same safe read path.

## Impact

Clients using the schema-1 API cannot reliably discover capabilities:

- Supported features are reported as `capability-unsupported`.
- `caps all` returns no capability map.
- Clients may disable valid controls or display many misleading errors.
- `doctor` reports a healthy installation despite the missing runtime element.

## Requested acceptance criteria

1. The Nix wrappers include the matching GStreamer core plugin directory.
2. `image status`, auto-framing, Whiteboard, DeskView, and gesture status succeed in a normal DMS/desktop environment.
3. `linkctl doctor` detects missing required GStreamer elements.
4. `caps all` returns a usable capability report when one capability cannot be read in the current stream state.
5. A directly readable `mode.compatibility` capability does not cause the entire aggregate request to fail.
