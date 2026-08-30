#!/usr/bin/env bash
set -euo pipefail

source_root=$(realpath "${1:-.}")
source_revision=${LINKCTL_SOURCE_REVISION:?set LINKCTL_SOURCE_REVISION to the immutable source revision}
source_date_epoch=${SOURCE_DATE_EPOCH:?set SOURCE_DATE_EPOCH to the source revision timestamp}

[[ $source_revision =~ ^[0-9a-f]{7,64}$ ]] || {
  echo "LINKCTL_SOURCE_REVISION must be a hexadecimal VCS revision" >&2
  exit 2
}
[[ $source_date_epoch =~ ^[0-9]+$ ]] || {
  echo "SOURCE_DATE_EPOCH must be an integer Unix timestamp" >&2
  exit 2
}

export CARGO_INCREMENTAL=0
export LINKCTL_SOURCE_REVISION="$source_revision"
export SOURCE_DATE_EPOCH="$source_date_epoch"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$source_root=/usr/src/linkctl"

cd "$source_root"
cargo build --locked --release \
  --package link-cli --bin linkctl \
  --package link-daemon --bin linkd
