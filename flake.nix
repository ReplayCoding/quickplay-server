{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, fenix }:
    let forAllSystems = nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed;
    in {
      # For `nix develop`:
      devShell = forAllSystems (system:
        let
	  pkgs = (import nixpkgs) { inherit system; };
          rust-pkgs = fenix.packages.${system}.stable;
        in pkgs.mkShell {
          nativeBuildInputs = with rust-pkgs; [ rustc cargo rustfmt clippy ];
          RUST_SRC_PATH = "${rust-pkgs.rust-src}/lib/rustlib/src/rust/library";
        });
    };
}

