# Use the same nixpkgs revision as the flake, without requiring nix develop.
{ pkgs ? import (let
    locked = (builtins.fromJSON (builtins.readFile ./flake.lock)).nodes.nixpkgs.locked;
  in builtins.fetchTarball {
    url = locked.url;
    sha256 = locked.narHash;
  }) { }
, rustToolchain ? null
}:
let
  runtimeLibraries = with pkgs; [
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
pkgs.mkShell {
  packages = [ pkgs.pkg-config ] ++ (
    if rustToolchain == null
    then (with pkgs; [ cargo rustc rustfmt clippy ])
    else [ rustToolchain ]
  );
  buildInputs = runtimeLibraries;
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;
}
