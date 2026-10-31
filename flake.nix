{
  description = "RLV Forge: native GPX editing and video alignment for indoor cycling";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      runtimeLibrariesFor = pkgs:
        pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux (with pkgs; [
          dbus
          libglvnd
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
          libxcb
        ]);
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.callPackage ./package.nix {
            runtimeLibraries = runtimeLibrariesFor pkgs;
            gitRevision = self.rev or self.dirtyRev or null;
          };
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/rlv-forge";
          meta.description = self.packages.${system}.default.meta.description;
        };
      });

      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          runtimeLibraries = runtimeLibrariesFor pkgs;
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
              pkg-config
              ffmpeg
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              pkgs.xorg-server
              pkgs.mesa
              pkgs.zenity
            ];
            buildInputs = runtimeLibraries;
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibraries}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            '';
          };
        });
    };
}
