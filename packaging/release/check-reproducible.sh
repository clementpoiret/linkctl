#!/usr/bin/env bash
set -euo pipefail

source_root=$(realpath "${1:-.}")
source_date_epoch=${SOURCE_DATE_EPOCH:?set SOURCE_DATE_EPOCH}
source_revision=${LINKCTL_SOURCE_REVISION:?set LINKCTL_SOURCE_REVISION}
temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT

mkdir -p "$temporary/archive-a" "$temporary/archive-b"
SOURCE_DATE_EPOCH="$source_date_epoch" \
  "$source_root/packaging/release/source-archive.sh" "$source_root" "$temporary/archive-a" >/dev/null
SOURCE_DATE_EPOCH="$source_date_epoch" \
  "$source_root/packaging/release/source-archive.sh" "$source_root" "$temporary/archive-b" >/dev/null
cmp "$temporary/archive-a"/*.tar.gz "$temporary/archive-b"/*.tar.gz

for build in a b; do
  CARGO_TARGET_DIR="$temporary/target-$build" \
    LINKCTL_SOURCE_REVISION="$source_revision" \
    SOURCE_DATE_EPOCH="$source_date_epoch" \
    "$source_root/packaging/release/build-binaries.sh" "$source_root"
done
cmp "$temporary/target-a/release/linkctl" "$temporary/target-b/release/linkctl"
cmp "$temporary/target-a/release/linkd" "$temporary/target-b/release/linkd"

echo "source archive and release binaries are byte-for-byte reproducible"
