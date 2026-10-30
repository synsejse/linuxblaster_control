{ lib
, rustPlatform
, pkg-config
, udev
, dbus
, wayland
, libxkbcommon
, libGL
, libglvnd
}:

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
      ../examples
      ../tests
      ../docs
      ../packaging
      ../README.md
      ../LICENSE
    ];
  };

  cargoLock = {
    lockFile = ../Cargo.lock;
  };

  nativeBuildInputs = [
    pkg-config
  ];

  buildInputs = [
    udev
    wayland
    libxkbcommon
    libGL
    libglvnd
  ];

  # Runtime library path for dynamic linking
  # (dbus is loaded at runtime by the file dialog for Load/Save Profile)
  runtimeDependencies = [
    udev
    dbus
    wayland
    libxkbcommon
    libGL
    libglvnd
  ];

  # Captured protocol tests and headless UI checks need no device or display.
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
    cp -r ${../docs} $out/share/doc/${pname}/docs
    install -Dm644 ${../LICENSE} $out/share/licenses/${pname}/LICENSE
  '';

  postFixup = ''
    # Add runtime dependencies to the binary
    patchelf --add-rpath "${lib.makeLibraryPath [ udev dbus wayland libxkbcommon libGL libglvnd ]}" $out/bin/linuxblaster_control
  '';

  meta = with lib; {
    description = "Native Linux GUI application to control the Creative Sound Blaster X4";
    longDescription = ''
    '';
    homepage = "https://github.com/synsejse/linuxblaster_control";
    license = licenses.mit;
    maintainers = [ ];
    mainProgram = "linuxblaster_control";
    platforms = platforms.linux;
  };
}

