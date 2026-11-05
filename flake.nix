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
    let
      tests = import ./nix/vendor/tests {
        inherit self;
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
      };
    in
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages.chipsmith-vendor = pkgs.callPackage ./nix/vendor/package.nix { };

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
            # for NixOS — see crates/vendor-quartus-common/src/nixos.rs
            patchelf
            bubblewrap
            unzip
          ];
        };
      }
    )
    // {
      nixosModules.chipsmith-vendor = import ./nix/vendor/module.nix { inherit self; };

      vendorLib = import ./nix/vendor/lib.nix { inherit self; };

      checks.x86_64-linux = nixpkgs.lib.mapAttrs' (n: nixpkgs.lib.nameValuePair "vendor-${n}") tests.checks;
      # e2e tests whose inputs nobody can fetch anonymously; see nix/tests
      legacyPackages.x86_64-linux = nixpkgs.lib.mapAttrs' (n: nixpkgs.lib.nameValuePair "vendor-${n}") tests.manual;
    };
}
