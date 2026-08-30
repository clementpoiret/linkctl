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
[[ $source_date_epoch =~ ^[0-9]+$ ]] || {
  echo "SOURCE_DATE_EPOCH must be an integer Unix timestamp" >&2
  exit 2
}

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_root/Cargo.toml" | head -1)
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "could not determine a release version from Cargo.toml" >&2
  exit 1
}

temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
file_list="$temporary/files"

if command -v jj >/dev/null && jj -R "$source_root" root >/dev/null 2>&1; then
  jj -R "$source_root" file list -r "${LINKCTL_ARCHIVE_REVISION:-@}" >"$file_list"
else
  git -C "$source_root" ls-files >"$file_list"
fi

LC_ALL=C sort -o "$file_list" "$file_list"
archive="$output_directory/linkctl-$version.tar.gz"
temporary_archive="$temporary/linkctl-$version.tar.gz"
tar --create \
  --directory "$source_root" \
  --no-recursion \
  --verbatim-files-from \
  --sort=name \
  --mtime="@$source_date_epoch" \
  --owner=0 --group=0 --numeric-owner \
  --mode='u+rwX,go+rX,go-w' \
  --transform="s,^,linkctl-$version/," \
  --file=- \
  --files-from "$file_list" \
  | gzip -9n >"$temporary_archive"
install -m644 "$temporary_archive" "$archive"

echo "$archive"
