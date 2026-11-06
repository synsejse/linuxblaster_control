{ lib
, rustPlatform
, pkg-config
, desktop-file-utils
, udev
, wayland
, libxkbcommon
, vulkan-loader
, libx11
, libxcursor
, libxi
, libxrandr
, libGL
, libglvnd
}:

let
  runtimeLibraries = [
    udev
    wayland
    libxkbcommon
    vulkan-loader
    libGL
    libglvnd
    libx11
    libxcursor
    libxi
    libxrandr
  ];
in
rustPlatform.buildRustPackage rec {
  pname = "linuxblaster_control";
  version = "0.1.0";

  # only what the build needs, so target/ and other local files stay out of the store
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../fixtures
      ../packaging
      ../README.md
      ../LICENSE
    ];
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
    outputHashes = import ../packaging/cargo-git-hashes.nix;
  };

  nativeBuildInputs = [
    pkg-config
    desktop-file-utils
  ];

  buildInputs = runtimeLibraries;

  # Headless UI checks need no device or display; the API has its own test suite.
  doCheck = true;

  postInstall = ''
    # Install udev rules
    install -Dm644 ${../packaging/70-linuxblaster-control.rules} $out/lib/udev/rules.d/70-linuxblaster-control.rules

    # Application menu entry and icons
    install -Dm644 ${../packaging/linuxblaster-control.desktop} $out/share/applications/linuxblaster-control.desktop
    install -Dm644 ${../packaging/linuxblaster-control.png} $out/share/icons/hicolor/256x256/apps/linuxblaster-control.png
    install -Dm644 ${../packaging/linuxblaster-control.svg} $out/share/icons/hicolor/scalable/apps/linuxblaster-control.svg

    # README and LICENSE
    install -Dm644 ${../README.md} $out/share/doc/${pname}/README.md
    install -Dm644 "$cargoDeps"/soundblaster-x4-*/README.md $out/share/doc/${pname}/api/README.md
    install -Dm644 "$cargoDeps"/soundblaster-x4-*/docs/PROTOCOL.md $out/share/doc/${pname}/api/PROTOCOL.md
    install -Dm644 ${../LICENSE} $out/share/licenses/${pname}/LICENSE
    install -Dm644 ${../packaging/licenses/inter/LICENSE.txt} $out/share/licenses/${pname}/inter/LICENSE.txt
    install -Dm644 ${../packaging/licenses/lucide/LICENSE} $out/share/licenses/${pname}/lucide/LICENSE

    desktop-file-validate $out/share/applications/linuxblaster-control.desktop
  '';

  postFixup = ''
    # Add runtime dependencies to the binary
    patchelf --add-rpath "${lib.makeLibraryPath runtimeLibraries}" $out/bin/linuxblaster_control
  '';

  meta = with lib; {
    description = "Native Linux GUI application to control the Creative Sound Blaster X4";
    homepage = "https://github.com/synsejse/linuxblaster_control";
    license = licenses.mit;
    maintainers = [ ];
    mainProgram = "linuxblaster_control";
    platforms = platforms.linux;
  };
}

