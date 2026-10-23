{
  description = "chipsmith: an open FPGA toolchain, HDL in, programmed chip out";

  inputs = {
    # The Cyclone II device database, techmap, packer and nextpnr uarch.
    cyclone2-re.url = "github:cramt/cyclone2-re";
    # One nixpkgs for both, so nextpnr-cyclone2 (a ~15 min chip database
    # build) is the same derivation cyclone2-re's devshell already built.
    nixpkgs.follows = "cyclone2-re/nixpkgs";
    flake-utils.follows = "cyclone2-re/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      cyclone2-re,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        open = cyclone2-re.packages.${system};
        flowTools = [
          pkgs.ghdl
          pkgs.yosys
          open.nextpnr-cyclone2
          open.cram
        ];
      in
      {
        packages.default = pkgs.callPackage ./nix/package.nix {
          inherit (open)
            nextpnr-cyclone2
            cram
            cyclone2-synth
            ;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.just
            pkgs.pkg-config
            pkgs.libusb1
          ]
          ++ flowTools;
          CHIPSMITH_CYCLONE2_SYNTH = open.cyclone2-synth;
        };
      }
    );
}
