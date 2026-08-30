{
  description = "Safe Linux control and media tools for Insta360 Link 2C Pro";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = function:
        builtins.listToAttrs (map (system: {
          name = system;
          value = function system;
        }) systems);
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          toolchain = pkgs.rust-bin.stable."1.97.1".minimal;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          rustTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget;
          gstPlugins = with pkgs.gst_all_1; [
            gst-plugins-base
            gst-plugins-good
            gst-plugins-bad
            gst-libav
          ];
          package = rustPlatform.buildRustPackage {
            pname = "linkctl";
            version = "1.0.0";
            src = self;

            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--package" "link-cli" "--bin" "linkctl"
              "--package" "link-daemon" "--bin" "linkd"
            ];
            cargoTestFlags = [ "--workspace" ];

            nativeBuildInputs = with pkgs; [
              clang
              help2man
              makeWrapper
              pkg-config
            ];
            buildInputs = with pkgs; [
              alsa-lib
              gst_all_1.gstreamer
              libclang
              pipewire
              systemd
            ] ++ gstPlugins;

            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "-I${pkgs.glibc.dev}/include";
            V4L2R_VIDEODEV2_H_PATH = "${pkgs.linuxHeaders}/include";
            LINKCTL_SOURCE_REVISION = self.rev or (self.dirtyRev or "unknown");
            SOURCE_DATE_EPOCH = toString (self.lastModified or 1);
            CARGO_INCREMENTAL = "0";
            RUSTFLAGS = "--remap-path-prefix=${self}=/usr/src/linkctl";

            installPhase = ''
              runHook preInstall
              LINKCTL_BINARY_DIR="$PWD/target/${rustTarget}/release" \
                LINKCTL_LINKD_PATH="$out/bin/linkd" \
                LINKCTL_PREFIX="" \
                bash packaging/common/install.sh "$PWD" "$out"
              runHook postInstall
            '';

            postFixup = ''
              pluginPath=${pkgs.lib.makeSearchPath "lib/gstreamer-1.0" gstPlugins}
              wrapProgram "$out/bin/linkctl" \
                --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "$pluginPath"
              wrapProgram "$out/bin/linkd" \
                --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "$pluginPath"
            '';

            meta = with pkgs.lib; {
              description = "Safe Linux control and media tools for Insta360 Link 2C Pro";
              homepage = "https://github.com/clementpoiret/linkctl";
              license = [ licenses.mit licenses.asl20 ];
              mainProgram = "linkctl";
              platforms = systems;
            };
          };
        in {
          default = package;
          linkctl = package;
        });
    };
}
