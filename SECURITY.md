# Security policy

## Supported versions

Security fixes are provided for the latest `1.0.x` release. Development snapshots and builds with `research` or
`network` enabled are outside the standard release security boundary.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting for this repository. Include the affected revision or package, Linux
distribution, camera firmware when relevant, a minimal reproduction, impact, and whether the report contains device
identifiers or captured media. Do not put secrets, serial numbers, unpublished device traces, firmware, or media in a
public issue.

If private vulnerability reporting is unavailable, open a public issue containing only a request for a private contact
channel. Do not include vulnerability details in that issue.

An initial acknowledgement is targeted within seven days. Validation and a remediation schedule depend on severity,
hardware availability, and whether the issue is in `linkctl`, a kernel driver, GStreamer, or camera firmware. Please
allow coordinated remediation before public disclosure.

## Security boundary

The standard package:

- runs `linkctl` and `linkd` as the logged-in user, with no setuid component;
- uses an owner-only Unix socket and verifies peer user IDs;
- has no network listener;
- permits vendor writes only from compiled-in, verified profiles matched to the exact descriptor and firmware;
- refuses unknown, firmware, boot, flash, calibration, reset, and mechanical XU writes;
- stages only an explicitly supplied firmware file through the documented manual U-Disk flow;
- redacts diagnostic bundles and never uploads media or reports automatically.

The physical shutter and microphone mute are independent. `linkctl` does not claim that logical stream state controls
the physical shutter. See the [threat model](docs/threat-model.md) for trust boundaries and abuse cases.

## Release integrity

Release artifacts are accompanied by `SHA256SUMS`, CycloneDX SBOMs, and `release-manifest.json`. The manifest binds the
source revision, build timestamp, schemas, standard feature set, artifact hashes, and compiled-in profile hashes.
GitHub artifact attestations are verified separately; a checksum alone does not establish who produced an artifact.
