# Contributing

## Environment and checks

Use `devenv` for every repository tool. Do not rely on a different host Rust toolchain or install project utilities globally.

```sh
devenv shell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo deny --all-features check
```

Keep changes narrow. Add hardware-free tests for parsing, schemas, codecs, and policy logic. Hardware tests must be explicitly classified by their side effects before they are introduced.

Local files under `specs/` are product inputs, not repository content. They are ignored by version control and must never be added to a change. Do not refer to delivery phase names in code, tests, or documentation.

## Evidence and reverse engineering

- Treat Linux runtime enumeration and target-device traces as authoritative.
- Treat mappings from other camera models as research leads only.
- Record the model, firmware, descriptor fingerprint, stream state, tool version, experiment steps, and observable result for proprietary-control evidence.
- Never brute-force hardware writes, detach `uvcvideo` in normal workflows, or publish unreviewed traces.
- Redact serial numbers, usernames, home paths, credentials, and captured media before sharing an artifact.

## Licenses and provenance

- Do not copy source code, payload tables, schemas, assets, model weights, or binaries without a compatible license and recorded provenance.
- Community projects may inform behavior, but reimplement documented kernel interfaces and independently observed behavior unless reuse is explicitly licensed.
- Do not redistribute Insta360 firmware, controller assets, private traces, or proprietary models.
- New dependencies must pass `cargo deny`; a new license or source exception requires an explicit review and a narrow comment in `deny.toml`.
- Contributions are accepted under the repository's `MIT OR Apache-2.0` dual-license terms.

## Unsafe Rust and native boundaries

Unsafe Rust is forbidden by default across the workspace. Low-level ioctl implementations must confine each exception to a dedicated backend module, document the kernel ABI invariant, and add structure-layout and malformed-input tests before relaxing that module's lint.

Hardware control tests must capture the current value immediately before a write and restore that exact value afterward, including automatic/manual parent controls. Start with `--dry-run`; never exercise pan, tilt, reset, firmware, calibration, or unknown XU writes. A test that cannot prove restoration is a manual experiment requiring an explicit review.
