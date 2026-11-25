{
  description = "Field-label capture GUI and photo-organizing CLI tools";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      pkgsFor = system: nixpkgs.legacyPackages.${system};
      guiLibs =
        pkgs: with pkgs; [
          libxkbcommon
          wayland
          libGL
          libx11
          libxcursor
          libxrandr
          libxi
        ];
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          label-qr-gui = pkgs.rustPlatform.buildRustPackage {
            pname = "label-qr-gui";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "label-qr-gui"
            ];
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.makeWrapper
            ];
            buildInputs = guiLibs pkgs;
            # winit's X11 backend dlopen()s libxkbcommon-x11 by name at
            # runtime rather than linking it, so patchelf's RPATH fixup
            # doesn't cover it. Wrap the binary with LD_LIBRARY_PATH so it
            # runs standalone (e.g. spawned directly from XMonad) without
            # needing `nix develop`.
            postFixup = ''
              wrapProgram $out/bin/label-qr-gui \
                --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath (guiLibs pkgs)}"
            '';
            meta = with pkgs.lib; {
              description = "Fullscreen field-label entry appliance with a large QR-code readout";
              homepage = "https://github.com/rickprice/field-label-tools";
              license = licenses.mit;
              mainProgram = "label-qr-gui";
            };
          };
          default = self.packages.${system}.label-qr-gui;
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              pkg-config
            ];
            buildInputs = guiLibs pkgs;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (guiLibs pkgs);
          };
        }
      );
    };
}
