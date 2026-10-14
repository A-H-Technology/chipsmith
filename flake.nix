{
  description = "chipsmith — declarative FPGA toolchain management and builds";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages.default = pkgs.callPackage ./nix/package.nix { };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt

            # `chipsmith test` looks these up on PATH rather than installing
            # them: the Quartus simulator needs a licence, GHDL needs nothing.
            ghdl
            just

            # what chipsmith shells out to when it patches a Quartus install
            # for NixOS — see crates/chipsmith-quartus-common/src/nixos.rs
            patchelf
            bubblewrap
            unzip
          ];
        };
      }
    )
    // {
      nixosModules.default = import ./nix/module.nix { inherit self; };

      lib = import ./nix/lib.nix { inherit self; };

      checks.x86_64-linux = import ./nix/tests {
        inherit self;
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
      };
    };
}
