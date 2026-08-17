{
  description = "rapid-analyzer -- review MAVLink tlog, SQLite sensor logs, video and audio on one synchronized timeline";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # alsa-lib (cpal/rodio) and the X11/Wayland stack make this a
      # Linux-targeted app; see the README's apt line for the same list.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Libraries the app `dlopen`s at run time instead of linking against:
      # wgpu loads Vulkan (and GL as a fallback), winit loads X11/Wayland and
      # xkbcommon. `ldd` on the built binary lists only libasound, so none of
      # these end up in its RPATH -- they have to be found via
      # LD_LIBRARY_PATH, hence the wrapper below.
      runtimeLibraries =
        pkgs: with pkgs; [
          vulkan-loader
          libGL
          libxkbcommon
          wayland
          xorg.libX11
          xorg.libXcursor
          xorg.libXi
          xorg.libXrandr
        ];

      # ffmpeg/ffprobe are invoked as subprocesses for video frames and audio
      # waveforms (src/import/{video,audio}.rs). The headless build keeps the
      # closure small and still ships both binaries.
      mediaTools = pkgs: [ pkgs.ffmpeg-headless ];

      mkPackage =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "rapid-analyzer";
          version = "0.1.0";

          # An explicit allowlist, not `lib.cleanSource ./.`: the working tree
          # carries a multi-gigabyte `target/` that must never be copied into
          # the nix store.
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./build.rs
              ./src
              ./examples
              # The headless UI tests, so `nix flake check` runs them too.
              ./tests
              # build.rs generates the `rapid` MAVLink dialect from these at
              # build time, so they are build inputs, not just docs.
              ./mavlink_dialects
            ];
          };

          # Every dependency resolves to crates.io, so the lockfile alone
          # pins the build -- no vendor hash to keep in sync.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
            # mavlink-bindgen shells out to rustfmt to format the dialect it
            # generates. It degrades to a cargo warning when absent, but the
            # generated source is far easier to read with it present.
            rustfmt
          ];

          buildInputs = [ pkgs.alsa-lib ];

          postInstall = ''
            wrapProgram $out/bin/rapid-analyzer \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibraries pkgs)} \
              --prefix PATH : ${pkgs.lib.makeBinPath (mediaTools pkgs)}
          '';

          meta = {
            description = "Synchronized multi-source (tlog / SQLite / video / audio) experiment data viewer";
            mainProgram = "rapid-analyzer";
            platforms = pkgs.lib.platforms.linux;
          };
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        rapid-analyzer = mkPackage pkgs;
        default = rapid-analyzer;
      });

      apps = forAllSystems (pkgs: rec {
        rapid-analyzer = {
          type = "app";
          program = pkgs.lib.getExe (mkPackage pkgs);
        };
        default = rapid-analyzer;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          # Reuses the package's pkg-config/alsa-lib/rustfmt/rustc/cargo.
          inputsFrom = [ (mkPackage pkgs) ];

          packages =
            (with pkgs; [
              clippy
              rust-analyzer
            ])
            ++ mediaTools pkgs;

          # `cargo run` produces an unwrapped binary, so the dev shell has to
          # supply the same dlopen search path the wrapper does. Prefixed
          # rather than assigned, to stay friendly on non-NixOS hosts.
          shellHook = ''
            export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath (runtimeLibraries pkgs)}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
          '';
        };
      });

      # `nix flake check` builds the package, which runs `cargo test` in its
      # check phase.
      checks = forAllSystems (pkgs: { rapid-analyzer = mkPackage pkgs; });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
