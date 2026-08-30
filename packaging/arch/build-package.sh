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
source_sha256=$(sha256sum "$source_archive" | cut -d' ' -f1)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_root/Cargo.toml" | head -1)

temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
install -m644 "$source_archive" "$temporary/linkctl-$version.tar.gz"
sed \
  -e "s/@SOURCE_SHA256@/$source_sha256/g" \
  -e "s/@SOURCE_REVISION@/$source_revision/g" \
  -e "s/@SOURCE_DATE_EPOCH@/$source_date_epoch/g" \
  "$source_root/packaging/arch/PKGBUILD.in" >"$temporary/PKGBUILD"

cd "$temporary"
PKGDEST="$output_directory" SRCDEST="$temporary" BUILDDIR="$temporary/build" \
  makepkg --clean --cleanbuild --force --noconfirm --noprogressbar \
    PKGEXT=.pkg.tar.zst
