# shell.nix
#
# This file defines a development environment with all the necessary
# dependencies to compile your Rust application.

{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  # The build inputs are the packages needed to build the project.
  buildInputs = with pkgs; [
    # Rust toolchain
    cargo
    rustc

    # System dependencies for the Rust crates
    pkg-config
    alsa-lib
    libevdev
  ];

  # You can add custom shell commands here if needed in the future.
  # shellHook = ''
  #   echo "Entering development environment for Key Counter Daemon..."
  # '';
}

