#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
  echo "usage: $0 SOURCE_ROOT OUTPUT_DIRECTORY" >&2
  exit 2
fi

source_root=$(realpath "$1")
output_directory=$2
mkdir -p "$output_directory"
output_directory=$(realpath "$output_directory")
source_date_epoch=${SOURCE_DATE_EPOCH:?set SOURCE_DATE_EPOCH to the source revision timestamp}
target=${LINKCTL_SBOM_TARGET:-all}

trap 'rm -f -- \
  "$source_root/crates/link-cli/linkctl_bin.cdx.json" \
  "$source_root/crates/link-daemon/linkd_bin.cdx.json"' EXIT

SOURCE_DATE_EPOCH="$source_date_epoch" cargo cyclonedx \
  --manifest-path "$source_root/crates/link-cli/Cargo.toml" \
  --format json --describe binaries --target "$target" \
  --spec-version 1.5 --license-strict \
  --license-accept-named Apache-2.0/MIT \
  --license-accept-named MIT/Apache-2.0
SOURCE_DATE_EPOCH="$source_date_epoch" cargo cyclonedx \
  --manifest-path "$source_root/crates/link-daemon/Cargo.toml" \
  --format json --describe binaries --target "$target" \
  --spec-version 1.5 --license-strict \
  --license-accept-named Apache-2.0/MIT \
  --license-accept-named MIT/Apache-2.0

install -Dm644 "$source_root/crates/link-cli/linkctl_bin.cdx.json" \
  "$output_directory/linkctl.cdx.json"
install -Dm644 "$source_root/crates/link-daemon/linkd_bin.cdx.json" \
  "$output_directory/linkd.cdx.json"
