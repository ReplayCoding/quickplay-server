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
        let pkgs = (import nixpkgs) { inherit system; };
        in pkgs.mkShell {
          nativeBuildInputs = with fenix.packages.${system}.stable; [ rustc cargo rustfmt clippy rust-analyzer ];
        });
    };
}

