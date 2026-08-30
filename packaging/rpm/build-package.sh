#!/usr/bin/env bash
set -euo pipefail

if (( $# != 3 )); then
  echo "usage: $0 SOURCE_ROOT SOURCE_ARCHIVE OUTPUT_DIRECTORY" >&2
  exit 2
fi

source_root=$(realpath "$1")
source_archive=$(realpath "$2")
output_directory=$3
mkdir -p "$output_directory"
output_directory=$(realpath "$output_directory")
source_revision=${LINKCTL_SOURCE_REVISION:?set LINKCTL_SOURCE_REVISION}
source_date_epoch=${SOURCE_DATE_EPOCH:?set SOURCE_DATE_EPOCH}
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_root/Cargo.toml" | head -1)

temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
mkdir -p "$temporary/SOURCES" "$temporary/SPECS"
install -m644 "$source_archive" "$temporary/SOURCES/linkctl-$version.tar.gz"
install -m644 "$source_root/packaging/rpm/linkctl.spec" "$temporary/SPECS/linkctl.spec"

rpmbuild -bb "$temporary/SPECS/linkctl.spec" \
  --define "_topdir $temporary" \
  --define "source_revision $source_revision" \
  --define "source_date_epoch $source_date_epoch"

find "$temporary/RPMS" -type f -name '*.rpm' -exec install -m644 -t "$output_directory" {} +
