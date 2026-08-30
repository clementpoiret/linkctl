#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
  echo "usage: $0 SOURCE_ROOT DESTDIR" >&2
  exit 2
fi

source_root=$(realpath "$1")
destdir=$2
mkdir -p "$destdir"
destdir=$(realpath "$destdir")
binary_dir=${LINKCTL_BINARY_DIR:-"$source_root/target/release"}
linkd_path=${LINKCTL_LINKD_PATH:-/usr/bin/linkd}
prefix=${LINKCTL_PREFIX-/usr}
unset LINKCTL_BINARY_DIR LINKCTL_LINKD_PATH LINKCTL_PREFIX

for command in help2man install sha256sum sort; do
  command -v "$command" >/dev/null || {
    echo "required command is unavailable: $command" >&2
    exit 1
  }
done
for binary in linkctl linkd; do
  test -x "$binary_dir/$binary" || {
    echo "release binary is unavailable: $binary_dir/$binary" >&2
    exit 1
  }
done

install -Dm755 "$binary_dir/linkctl" "$destdir$prefix/bin/linkctl"
install -Dm755 "$binary_dir/linkd" "$destdir$prefix/bin/linkd"

generated=$(mktemp -d)
trap 'rm -rf -- "$generated"' EXIT

help2man --no-info --section=1 --name='control and inspect an Insta360 Link 2C Pro' \
  "$binary_dir/linkctl" >"$generated/linkctl.1"
help2man --no-info --section=1 --name='per-user linkctl camera daemon' \
  "$binary_dir/linkd" >"$generated/linkd.1"
install -Dm644 "$generated/linkctl.1" "$destdir$prefix/share/man/man1/linkctl.1"
install -Dm644 "$generated/linkd.1" "$destdir$prefix/share/man/man1/linkd.1"

"$binary_dir/linkctl" completion bash >"$generated/linkctl.bash"
"$binary_dir/linkctl" completion zsh >"$generated/_linkctl"
"$binary_dir/linkctl" completion fish >"$generated/linkctl.fish"
"$binary_dir/linkctl" completion elvish >"$generated/linkctl.elv"
install -Dm644 "$generated/linkctl.bash" \
  "$destdir$prefix/share/bash-completion/completions/linkctl"
install -Dm644 "$generated/_linkctl" "$destdir$prefix/share/zsh/site-functions/_linkctl"
install -Dm644 "$generated/linkctl.fish" \
  "$destdir$prefix/share/fish/vendor_completions.d/linkctl.fish"
install -Dm644 "$generated/linkctl.elv" "$destdir$prefix/share/elvish/lib/linkctl.elv"

install -Dm644 "$source_root/packaging/systemd/linkd.service" \
  "$destdir$prefix/lib/systemd/user/linkd.service"
if [[ $linkd_path != /usr/bin/linkd ]]; then
  sed -i "s|^ExecStart=/usr/bin/linkd$|ExecStart=$linkd_path|" \
    "$destdir$prefix/lib/systemd/user/linkd.service"
fi
install -Dm644 "$source_root/packaging/udev/70-linkctl.rules" \
  "$destdir$prefix/lib/udev/rules.d/70-linkctl.rules"

for document in \
  README.md \
  CHANGELOG.md \
  CONTRIBUTING.md \
  SECURITY.md \
  LICENSE-MIT \
  LICENSE-APACHE; do
  install -Dm644 "$source_root/$document" \
    "$destdir$prefix/share/doc/linkctl/$document"
done
while IFS= read -r -d '' document; do
  install -Dm644 "$source_root/$document" \
    "$destdir$prefix/share/doc/linkctl/$document"
done < <(
  cd "$source_root"
  find docs -type f \( -name '*.md' -o -name '*.json' \) -print0 \
    | LC_ALL=C sort -z
)
install -Dm644 "$source_root/LICENSE-MIT" "$destdir$prefix/share/licenses/linkctl/LICENSE-MIT"
install -Dm644 "$source_root/LICENSE-APACHE" \
  "$destdir$prefix/share/licenses/linkctl/LICENSE-APACHE"

(
  cd "$source_root"
  find profiles -type f -name '*.toml' -print0 \
    | LC_ALL=C sort -z \
    | xargs -0 sha256sum
) >"$generated/profiles.sha256"
install -Dm644 "$generated/profiles.sha256" \
  "$destdir$prefix/share/linkctl/profiles.sha256"
