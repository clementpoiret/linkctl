# Release runbook

This runbook prepares and publishes a release only after the maintainer has reviewed every gate. Repository commands
below are examples for the release operator; packaging or validation does not run them automatically.

## Preconditions

- The working revision contains only the intended release change, has no conflicts, and has a signed description.
- `CHANGELOG.md` has a real release date, Debian changelog timestamps are strictly descending, and no release blocker
  remains unresolved.
- Required CI, native package, reproducibility, security, and hardware-validation results are attached to the revision.
- The version in `Cargo.toml`, package recipes, and the requested tag is identical.
- No firmware, private trace, camera serial, credential, proprietary model, or captured media is present.

Inspect before doing anything remote:

```sh
jj --no-pager --color=never status
jj --no-pager --color=never diff --git -r @
jj --no-pager --color=never log -r 'trunk()..@' \
  -T 'change_id.short() ++ " " ++ commit_id.short() ++ " " ++ description.first_line() ++ "\n"'
jj --no-pager --color=never tag list --all-remotes
```

## Local release verification

Use the project environment and an immutable hexadecimal release revision:

```sh
export LINKCTL_SOURCE_REVISION=<full-release-commit-id>
export SOURCE_DATE_EPOCH=<release-commit-unix-timestamp>
devenv shell -- cargo fmt --all -- --check
devenv shell -- cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
devenv shell -- cargo test --workspace --all-features --locked
devenv shell -- cargo deny --all-features check
devenv shell -- cargo audit
devenv shell -- packaging/release/check-reproducible.sh .
```

Build packages only in the matching distribution runners or containers. Verify install, file ownership, service
hardening, read-only diagnostics, stop/start behavior, and clean removal. The package must not enable or start the user
service. Generate the source archive, two CycloneDX SBOMs, `release-manifest.json`, and `SHA256SUMS` last so their
hashes cover every package artifact.

## Manual GitHub workflow

Run the `Release candidate` workflow for the exact release revision. Enter `v1.0.2` and the confirmation text
`release-v1.0.2`. Leave `publish` false for a candidate-only run. Download all artifacts, compare their manifest and
checksums, and retain the workflow URL as evidence.

Only after a second explicit publication decision should the maintainer rerun the workflow with `publish` true. That
job uses GitHub's OIDC identity with `actions/attest@v4`, creates attestations for the immutable artifacts, and uploads
them to the already reviewed tag. It does not create or move the tag.

## Exact tag and push

Create the local tag only after replacing `<release-revision>` with the reviewed immutable revision:

```sh
jj tag set v1.0.2 --revision <release-revision>
jj --no-pager --color=never tag list --all-remotes
jj git push --dry-run --remote origin --tag 'exact:v1.0.2'
```

Read the complete dry-run output and confirm that it selects only `v1.0.2` at the intended revision. Publication then
requires a separate explicit command:

```sh
jj git push --remote origin --tag 'exact:v1.0.2'
jj --no-pager --color=never tag list --all-remotes
```

Never use a bare push, `--all`, `--tracked`, or `--deleted` for this operation. Never use `--allow-move` for an existing
release tag without a separately documented incident decision.

## Verify published artifacts

After GitHub publication, use a clean directory:

```sh
gh release download v1.0.2 --repo clementpoiret/linkctl --dir linkctl-1.0.2
cd linkctl-1.0.2
sha256sum --check SHA256SUMS
gh attestation verify --repo clementpoiret/linkctl ./*.deb
gh attestation verify --repo clementpoiret/linkctl ./*.rpm
gh attestation verify --repo clementpoiret/linkctl ./*.pkg.tar.zst
gh attestation verify --repo clementpoiret/linkctl ./linkctl-1.0.2.tar.gz
```

Confirm `release-manifest.json` has the tagged source revision, expected schemas, standard features, supported targets,
and profile hashes. Install one artifact per distribution from the downloaded set and repeat `linkctl --version`,
`linkctl doctor`, service start/status/stop, and removal smoke tests.

## Abort conditions

Stop without tagging or publishing if a checksum differs, a build lacks the source revision, a package comes from the
wrong distribution, a schema changes without review, a writable profile hash is unexpected, a security check reports
an unresolved high-severity issue, or mandatory hardware validation cannot restore the camera's starting state.
