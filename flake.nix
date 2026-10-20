{
  description = "oxide packages for Nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      legacyPackages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          oxide = pkgs.callPackage ./nix/package.nix { };
        }
      );

      packages = forAllSystems (
        system:
        let
          package = self.legacyPackages.${system}.oxide;
        in
        {
          default = package;
          oxide = package;
        }
      );

      apps = forAllSystems (
        system:
        let
          package = self.packages.${system}.oxide;
        in
        {
          default = {
            type = "app";
            program = "${package}/bin/oxide-native";
          };
          cli = {
            type = "app";
            program = "${package}/bin/oxide";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            name = "oxide-dev-shell";
            packages =
              with pkgs;
              [
                cargo
                clippy
                clang
                cmake
                git
                just
                makeWrapper
                nasm
                perl
                pkg-config
                rustc
                rustfmt
                libclang
              ]
              ++ (with pkgs; [
                alsa-lib
                dbus
                fontconfig
                freetype
                gst_all_1.gst-plugins-base
                gst_all_1.gst-plugins-good
                gst_all_1.gst-libav
                gst_all_1.gstreamer
                krb5
                libGL
                libunwind
                libx11
                libxcb
                libxcursor
                libxfixes
                libxinerama
                libxkbcommon
                libxrandr
                openh264
                openssl
                udev
                vulkan-loader
                wayland
              ]);
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
          };
        }
      );

      overlays.default = final: _prev: {
        oxide = final.callPackage ./nix/package.nix { };
      };

      nixosModules.oxide =
        { lib, pkgs, ... }:
        let
          system = pkgs.stdenv.hostPlatform.system;
          package =
            self.packages.${system}.oxide
              or (throw "oxide NixOS module does not support system '${system}'");
        in
        {
          imports = [ ./nix/module.nix ];
          programs.oxide.package = lib.mkDefault package;
        };
      nixosModules.default = self.nixosModules.oxide;

      checks = forAllSystems (system: {
        oxide = self.packages.${system}.oxide;
      });
    };
}
