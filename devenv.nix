{ pkgs, ... }:
{
  languages.rust.enable = true;

  env.LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
  env.BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.glibc.dev}/include";
  env.V4L2R_VIDEODEV2_H_PATH = "${pkgs.linuxHeaders}/include";

  packages = [
    pkgs.alsa-lib
    pkgs.alsa-utils
    pkgs.cargo-deny
    pkgs.glibc.dev
    pkgs.libclang
    pkgs.linuxHeaders
    pkgs.pkg-config
    pkgs.pipewire
    pkgs.systemd
    pkgs.usbutils
    pkgs.v4l-utils
    pkgs.wireplumber
  ];
}
