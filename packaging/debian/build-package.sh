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
source_date_epoch=${SOURCE_DATE_EPOCH:?set SOURCE_DATE_EPOCH}

"$source_root/packaging/release/build-binaries.sh" "$source_root"

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_root/Cargo.toml" | head -1)
architecture=$(dpkg --print-architecture)
target_directory=${CARGO_TARGET_DIR:-"$source_root/target"}
temporary=$(mktemp -d)
trap 'rm -rf -- "$temporary"' EXIT
package_root="$temporary/linkctl"

LINKCTL_BINARY_DIR="$target_directory/release" \
  "$source_root/packaging/common/install.sh" "$source_root" "$package_root"
install -Dm644 "$source_root/packaging/debian/copyright" \
  "$package_root/usr/share/doc/linkctl/copyright"
gzip -9n <"$source_root/packaging/debian/changelog" \
  >"$package_root/usr/share/doc/linkctl/changelog.Debian.gz"
gzip -9n "$package_root/usr/share/man/man1/linkctl.1" \
  "$package_root/usr/share/man/man1/linkd.1"

installed_size=$(du -sk "$package_root" | cut -f1)
mkdir -p "$package_root/DEBIAN"
cat >"$package_root/DEBIAN/control" <<EOF
Package: linkctl
Version: $version-1
Section: video
Priority: optional
Architecture: $architecture
Maintainer: Clément Poiret <clement@linux.com>
Installed-Size: $installed_size
Depends: libc6, libasound2t64, libgstreamer1.0-0 (>= 1.26), libpipewire-0.3-0t64, libudev1, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-libav
Homepage: https://github.com/clementpoiret/linkctl
Description: safe Linux control and media tools for Insta360 Link 2C Pro
 linkctl provides capability-driven camera control, capture, recording,
 diagnostics, and a per-user local stream daemon.
EOF

find "$package_root" -exec touch -h --date="@$source_date_epoch" {} +
dpkg-deb --build --root-owner-group --uniform-compression -Zxz \
  "$package_root" "$output_directory/linkctl_${version}-1_${architecture}.deb"
