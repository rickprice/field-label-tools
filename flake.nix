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
          # cpal (ltc.rs's timecode audio output) links ALSA on Linux.
          alsa-lib
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
          photo-splitter = pkgs.rustPlatform.buildRustPackage {
            pname = "photo-splitter";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "photo-splitter"
            ];
            nativeBuildInputs = [
              pkgs.pkg-config
              pkgs.rustPlatform.bindgenHook
              pkgs.makeWrapper
            ];
            buildInputs = [ pkgs.libraw ];
            # libraw_r links libgomp, so by default each rayon worker
            # thread's LibRaw call spawns its own OpenMP thread team --
            # 12 rayon workers x ~12 OpenMP threads each oversubscribes a
            # 12-core machine ~12x. OMP_NUM_THREADS=1 leaves rayon's
            # cross-file parallelism as the only parallelism layer.
            # --set-default so an explicit OMP_NUM_THREADS in the caller's
            # environment still wins.
            #
            # ffmpeg-headless (video.rs) is a *runtime* dependency, not a
            # build-time one -- it's shelled out to, never linked against
            # -- so it's put on PATH here rather than in buildInputs.
            # -headless specifically to skip the GUI/X11/Qt dependencies
            # the full ffmpeg package pulls in, which this CLI tool never
            # uses.
            postFixup = ''
              wrapProgram $out/bin/photo-splitter \
                --set-default OMP_NUM_THREADS 1 \
                --prefix PATH : "${pkgs.lib.makeBinPath [ pkgs.ffmpeg-headless ]}"
            '';
            meta = with pkgs.lib; {
              description = "Splits a directory of time-ordered photos into per-shoot folders using QR-code marker frames";
              homepage = "https://github.com/rickprice/field-label-tools";
              license = licenses.mit;
              mainProgram = "photo-splitter";
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
              rustPlatform.bindgenHook
              ffmpeg-headless
            ];
            buildInputs = guiLibs pkgs ++ [ pkgs.libraw ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (guiLibs pkgs);
            # See photo-splitter's postFixup above for why.
            OMP_NUM_THREADS = "1";
          };
        }
      );
    };
}
