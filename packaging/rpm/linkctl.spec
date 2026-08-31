Name:           linkctl
Version:        1.0.2
Release:        1%{?dist}
Summary:        Safe Linux control and media tools for Insta360 Link 2C Pro
License:        MIT OR Apache-2.0
URL:            https://github.com/clementpoiret/linkctl
Source0:        https://github.com/clementpoiret/linkctl/archive/refs/tags/v%{version}/linkctl-%{version}.tar.gz

BuildRequires:  alsa-lib-devel
BuildRequires:  cargo >= 1.97.1
BuildRequires:  clang-devel
BuildRequires:  gstreamer1-devel >= 1.26
BuildRequires:  gstreamer1-plugins-base-devel >= 1.26
BuildRequires:  help2man
BuildRequires:  pipewire-devel
BuildRequires:  pkgconf-pkg-config
BuildRequires:  rust >= 1.97.1
BuildRequires:  systemd-devel
Requires:       gstreamer1 >= 1.26
Requires:       gstreamer1-plugin-libav
Requires:       gstreamer1-plugins-bad-free
Requires:       gstreamer1-plugins-base
Requires:       gstreamer1-plugins-good
Requires:       systemd-udev

%{!?source_revision:%global source_revision unknown}
%global debug_package %{nil}

%description
linkctl provides capability-driven camera control, capture, recording,
diagnostics, and a per-user local stream daemon.

%prep
%autosetup

%build
export CARGO_INCREMENTAL=0
export LINKCTL_SOURCE_REVISION=%{source_revision}
export SOURCE_DATE_EPOCH=%{?source_date_epoch}
export RUSTFLAGS="--remap-path-prefix=%{_builddir}/linkctl-%{version}=/usr/src/linkctl"
cargo build --locked --release \
  --package link-cli --bin linkctl \
  --package link-daemon --bin linkd

%check
cargo test --locked --workspace

%install
LINKCTL_BINARY_DIR=%{_builddir}/linkctl-%{version}/target/release \
  bash packaging/common/install.sh %{_builddir}/linkctl-%{version} %{buildroot}

%files
%license %{_licensedir}/linkctl/LICENSE-APACHE
%license %{_licensedir}/linkctl/LICENSE-MIT
%doc %{_docdir}/linkctl
%{_bindir}/linkctl
%{_bindir}/linkd
%{_mandir}/man1/linkctl.1*
%{_mandir}/man1/linkd.1*
%{_datadir}/bash-completion/completions/linkctl
%{_datadir}/zsh/site-functions/_linkctl
%{_datadir}/fish/vendor_completions.d/linkctl.fish
%{_datadir}/elvish/lib/linkctl.elv
%{_datadir}/linkctl/profiles.sha256
%{_prefix}/lib/systemd/user/linkd.service
%{_prefix}/lib/udev/rules.d/70-linkctl.rules

%changelog
* Mon Aug 31 2026 Clément Poiret <clement@linux.com> - 1.0.2-1
- Reduce daemon power use, media fan-out overhead, and device polling

* Mon Aug 31 2026 Clément Poiret <clement@linux.com> - 1.0.1-1
- Fix the Nix GStreamer runtime and camera-native capability reporting

* Sun Aug 30 2026 Clément Poiret <clement@linux.com> - 1.0.0-1
- Initial package
