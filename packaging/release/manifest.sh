#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
  echo "usage: $0 SOURCE_ROOT ARTIFACT_DIRECTORY" >&2
  exit 2
fi

source_root=$(realpath "$1")
artifact_directory=$(realpath "$2")
source_revision=${LINKCTL_SOURCE_REVISION:?set LINKCTL_SOURCE_REVISION}
source_date_epoch=${SOURCE_DATE_EPOCH:?set SOURCE_DATE_EPOCH}
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_root/Cargo.toml" | head -1)

artifact_records=$(
  find "$artifact_directory" -maxdepth 1 -type f \
    ! -name SHA256SUMS ! -name release-manifest.json -print0 \
    | LC_ALL=C sort -z \
    | while IFS= read -r -d '' artifact; do
        jq -n \
          --arg name "$(basename "$artifact")" \
          --arg sha256 "$(sha256sum "$artifact" | cut -d' ' -f1)" \
          --argjson bytes "$(stat -c '%s' "$artifact")" \
          '{name: $name, bytes: $bytes, sha256: $sha256}'
      done \
    | jq -s .
)

profile_records=$(
  cd "$source_root"
  find profiles -type f -name '*.toml' -print0 \
    | LC_ALL=C sort -z \
    | while IFS= read -r -d '' profile; do
        jq -n \
          --arg path "$profile" \
          --arg trust "$(basename "$(dirname "$profile")")" \
          --arg sha256 "$(sha256sum "$profile" | cut -d' ' -f1)" \
          '{path: $path, trust: $trust, sha256: $sha256}'
      done \
    | jq -s .
)

jq -S -n \
  --arg version "$version" \
  --arg source_revision "$source_revision" \
  --argjson source_date_epoch "$source_date_epoch" \
  --argjson artifacts "$artifact_records" \
  --argjson profiles "$profile_records" \
  '{
    schema_version: 1,
    version: $version,
    source_revision: $source_revision,
    source_date_epoch: $source_date_epoch,
    rust_toolchain: "1.97.1",
    output_schema_version: 1,
    daemon_protocol_version: 1,
    standard_features: ["daemon", "gstreamer", "pipewire"],
    excluded_features: ["network", "research"],
    supported_targets: [
      "debian-13-amd64",
      "debian-13-arm64",
      "fedora-44-x86_64",
      "fedora-44-aarch64",
      "arch-x86_64",
      "archlinuxarm-aarch64",
      "nixos-26.05-x86_64-linux",
      "nixos-26.05-aarch64-linux"
    ],
    profiles: $profiles,
    artifacts: $artifacts
  }' >"$artifact_directory/release-manifest.json"

(
  cd "$artifact_directory"
  find . -maxdepth 1 -type f ! -name SHA256SUMS -printf '%f\0' \
    | LC_ALL=C sort -z \
    | xargs -0 sha256sum
) >"$artifact_directory/SHA256SUMS"
