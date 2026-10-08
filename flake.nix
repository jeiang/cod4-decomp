{
  description = "CoD4 multiplayer engine reimplementation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAll =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      # rust-toolchain.toml is the single toolchain pin; Windows CI reads it through rustup.
      devShells = forAll (pkgs: {
        default =
          let
            # winit and wgpu load these at run time.
            windowLibs = with pkgs; [
              libxkbcommon
              vulkan-loader
              wayland
              libx11
              libxcursor
              libxi
              libxrandr
            ];
            # The CLI must match the wasm-bindgen crate in Cargo.lock exactly; nixpkgs lags it.
            wasm-bindgen-cli = pkgs.buildWasmBindgenCli rec {
              src = pkgs.fetchCrate {
                pname = "wasm-bindgen-cli";
                version = "0.2.129";
                hash = "sha256-pcecKQd7E8Opw6bkFoE569epUi7gh5qpQF1e5PJY6V8=";
              };
              cargoDeps = pkgs.rustPlatform.fetchCargoVendor {
                inherit src;
                inherit (src) pname version;
                hash = "sha256-vmUrWVU7kPJJxO5qIVeAkwQyWDELO1Z4Z5gitz2kco8=";
              };
            };
          in
          pkgs.mkShell {
            packages = [
              (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
              pkgs.nasm # rav1e x86_64 asm
              wasm-bindgen-cli # web/ build
              pkgs.binaryen # wasm-opt for web/ release builds
              pkgs.python3 # web/serve.py
            ]
            # gilrs (gamepads) links libudev on Linux.
            ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
              pkgs.pkg-config
              pkgs.udev
              # cpal (sound) links ALSA on Linux.
              pkgs.alsa-lib
            ];
            LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux (pkgs.lib.makeLibraryPath (windowLibs ++ [ pkgs.udev pkgs.alsa-lib ]));
          };
      });
      formatter = forAll (pkgs: pkgs.nixfmt);
    };
}
