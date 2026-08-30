# Specification coverage

This matrix turns every retained product requirement into a release claim. “Partial” means the implemented subset is
usable and documented while the remaining behavior is explicitly unavailable. “Experimental” means absent from the
standard package. Hardware claims apply only to the device and firmware in the compatibility matrix.

| Requirements | Status | Release evidence or boundary |
|---|---|---|
| DEV-001–004 | implemented | udev/sysfs discovery, detailed inventory, hotplug JSONL, redacted probe and fixtures |
| DEV-005 | partial | multiple cameras are enumerated; one explicit camera is mutated and one camera is supervised per daemon |
| CTRL-001–005 | implemented | live extended-control descriptors, typed writes, batching, dependency re-query, readback and rollback |
| VID-001–005 | implemented | exact runtime tuples, try/set/readback, transport policy, and distinct native/host portrait reporting |
| IMG-001–005 | implemented | standard and verified-profile semantics, raw/normalized values, HDR guards, and local preset subsets |
| FRM-001 | implemented | standard 1×–4× zoom with bounded ramp and restoration |
| FRM-002 | partial | host graph crop/position exists; no camera-native translation mapping is claimed |
| FRM-003 | implemented | verified camera-native Auto Framing, Smart Composition prerequisite, and Head/Half-body style |
| FRM-004–005 | unavailable | no host detector, tracker, active-speaker policy, ROI, smoothing, or quality-control engine |
| MODE-001–002 | implemented | normal mode policy and verified camera-resident Whiteboard |
| MODE-003 | unavailable | Smart Whiteboard detection, calibration, rectification, freeze, and OCR are not present |
| MODE-004 | partial | verified camera-native DeskView exists; host calibration/perspective correction is unavailable |
| MODE-005 | unavailable | no host document geometry pipeline |
| MODE-006 | partial | native portrait is verified; host portrait orchestration is unavailable |
| MODE-007 | partial | verified native conflicts are rejected; no composition policy exists for unavailable host modes |
| AUD-001–004, AUD-006 | implemented | PipeWire/ALSA discovery and control, capture/monitoring, verified pickup modes, mux timing and drift reporting |
| AUD-005 | partial | fixed gate/compressor/limiter and resampling exist; EQ, AGC, echo cancellation, and advanced DSP do not |
| GEST-001 | implemented | verified independent Palm/V-sign/L-sign configuration with readback and restoration |
| GEST-002 | unavailable | no host gesture recognizer or action binder |
| GEST-003–004 | implemented | physical interaction is documented; logical state never claims touch or LED control/readback |
| PRIV-001 | implemented | read-only status separates shutter uncertainty, stream state, and hardware/host audio mute |
| PRIV-002–003 | unavailable | no logical privacy enter/exit or audit transition; status makes this absence explicit |
| PRE-001–005 | implemented | strict local presets for implemented groups, selective capture, dry-run plans, journals, rollback, no inline secrets |
| MEDIA-001 | implemented | direct/daemon JPEG, PNG, raw snapshots, stdout, metadata, burst and interval behavior |
| MEDIA-002 | partial | Matroska/MP4, pass-through, segmentation, rolling limits and guards exist; pre-event and chapter markers do not |
| MEDIA-003 | implemented | typed binary stdout with diagnostics separated and broken-pipe handling |
| MEDIA-004 | experimental | optional typed RTP/UDP exists; RTSP, SRT, integrated WebRTC, and gateways are unavailable |
| MEDIA-005 | unavailable | no timelapse or trigger engine |
| VCAM-001, VCAM-003–004, VCAM-007 | implemented | single-source bounded fan-out, explicit output contracts, base transforms, graph and metrics |
| VCAM-002 | affected | v4l2loopback production is implemented; OBS/WebRTC is unsupported on tested v4l2loopback 0.15.4 |
| VCAM-005–006 | unavailable | no background, segmentation, appearance, face, or relighting effects |
| OPS-001 | partial | authenticated protocol v1 covers local controls and media graph; presets, effect management, and event subscriptions are not all daemon operations |
| OPS-002 | unavailable | no D-Bus, HTTP, WebSocket, Prometheus, MQTT, MIDI, or OSC facade |
| OPS-003 | implemented | generated Bash/Zsh/Fish/Elvish completions, manuals, user service, and udev rules |
| OPS-004 | partial | device/control/recording/pipeline/firmware events exist; unavailable modes and privacy transitions emit none |
| XU-001–006 | implemented | descriptor-safe reads, snapshots/diffs, compiled-in profile writes, gated research writes, and redacted bundles |
| XU-007 | implemented | bounded reopen, graph rebuild, hotplug recovery, and physical-reconnect guidance; USB reset stays prohibited |
| FW-001–004 | implemented | safe version reporting, U-Disk detection, no-clobber manual staging, and refusal of forced boot/flash controls |
| NFR-001–002 | implemented | guarded writes, normal-user execution, narrow udev matching, no setuid or root runtime |
| NFR-003 | partial | 4K30 and 1080p60 endurance, A/V drift, and direct/graph latency targets are measured; affected vcam interoperability remains recorded separately |
| NFR-004 | implemented | bounded recovery, reconnect supervision, transaction journals, and finalized media shutdown paths |
| NFR-005 | partial | structured logs and local metrics exist; Prometheus export is unavailable |
| NFR-006–008 | implemented | pinned compatibility floor, schema/protocol v1, lockfile, deterministic metadata, source revision and profile hashes |

The standard release never turns an unavailable or experimental row into an inferred capability. `caps all`, command
availability, package features, and the compatibility matrix use the same boundary.
