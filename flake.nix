{
  description = "Linuxblaster Control - A native GUI application to control the Creative Sound Blaster X4";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:

    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                inputs.self.overlays.default
              ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        rustToolchain =
          with inputs.fenix.packages.${prev.stdenv.hostPlatform.system};
          combine (
            with complete;
            [
              clippy
              rustc
              cargo
              rustfmt
              rust-src
            ]
          );

        blaster-x4-control = prev.callPackage ./nix/package.nix {
          rustPlatform = prev.makeRustPlatform {
            cargo = final.rustToolchain;
            rustc = final.rustToolchain;
          };
        };
      };

      packages = forEachSupportedSystem (
        { pkgs }:
        {
          default = pkgs.blaster-x4-control;
          blaster-x4-control = pkgs.blaster-x4-control;
          # .deb/.rpm/AppImage packages are built by packaging/build-release.sh
        }
      );

      devShells = forEachSupportedSystem (
        { pkgs }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              udev
              dbus
              rustToolchain
              openssl
              pkg-config
              cargo-deny
              cargo-edit
              cargo-watch
              rust-analyzer
              bacon
              evcxr

              # wayland support
              wayland
              libxkbcommon
              libGL
              libglvnd

            ];

            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
              LD_LIBRARY_PATH = "${pkgs.lib.makeLibraryPath [ pkgs.udev pkgs.dbus pkgs.wayland pkgs.libxkbcommon pkgs.libGL pkgs.libglvnd pkgs.openssl]}";
            };
          };
        }
      );

      # NixOS module for easy installation
      nixosModules.default = { config, lib, pkgs, ... }:
        with lib;
        let
          cfg = config.hardware.soundblaster-x4;
        in
        {
          options.hardware.soundblaster-x4 = {
            enable = mkEnableOption "Sound Blaster X4 support";
            commandName = mkOption {
              type = types.str;
              default = "linuxblaster";
              description = "The command name to make available in the system path.";
            };
          };

          config = mkIf cfg.enable {
            # Install the control application
            environment.systemPackages = [
              self.packages.${pkgs.stdenv.hostPlatform.system}.blaster-x4-control
              (pkgs.runCommand "linuxblaster-symlink" {} ''
                mkdir -p $out/bin
                ln -s ${self.packages.${pkgs.stdenv.hostPlatform.system}.blaster-x4-control}/bin/linuxblaster_control $out/bin/${cfg.commandName}
              '')
            ];

            # udev rule for device access (lib/udev/rules.d in the package)
            services.udev.packages = [ self.packages.${pkgs.stdenv.hostPlatform.system}.blaster-x4-control ];
          };
        };
    };
}
