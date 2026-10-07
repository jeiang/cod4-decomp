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
        default = pkgs.mkShell {
          packages = [ (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml) ];
        };
      });
      formatter = forAll (pkgs: pkgs.nixfmt);
    };
}
