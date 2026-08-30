{ pkgs, ... }:
{
  languages.rust.enable = true;

  env.LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
  env.BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.glibc.dev}/include";
  env.V4L2R_VIDEODEV2_H_PATH = "${pkgs.linuxHeaders}/include";

  packages = [
    pkgs.actionlint
    pkgs.alsa-lib
    pkgs.alsa-utils
    pkgs.cargo-audit
    pkgs.cargo-cyclonedx
    pkgs.cargo-deny
    pkgs.dpkg
    pkgs.glibc.dev
    pkgs.gst_all_1.gstreamer
    pkgs.gst_all_1.gst-plugins-base
    pkgs.gst_all_1.gst-plugins-good
    pkgs.gst_all_1.gst-plugins-bad
    pkgs.gst_all_1.gst-libav
    pkgs.help2man
    pkgs.jq
    pkgs.libclang
    pkgs.linuxHeaders
    pkgs.pkg-config
    pkgs.pipewire
    pkgs.rpm
    pkgs.shellcheck
    pkgs.systemd
    pkgs.usbutils
    pkgs.v4l-utils
    pkgs.wireshark-cli
    pkgs.wireplumber
  ];

  profiles.vcam-test.module = { pkgs, ... }: {
    packages = [
      pkgs.chromium
      pkgs.obs-studio
    ];
  };
}
