{
  description = "chipsmith: an open FPGA toolchain, HDL in, programmed chip out";

  inputs = {
    # The Cyclone II device database (cram's db/), techmap and cram.
    cyclone2-re.url = "github:cramt/cyclone2-re";
    # One nixpkgs for both, so cram and its tools are the same derivations
    # cyclone2-re's devshell already built.
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
          open.cram
        ];
        chipsmith = pkgs.callPackage ./nix/package.nix {
          inherit (open)
            cram
            cyclone2-synth
            ;
        };
      in
      {
        packages.default = chipsmith;

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
          CHIPSMITH_CYCLONE2_DB = "${open.cram}/db";
          # Written on first use (and whenever the format changes), so the
          # shell doesn't depend on building chipsmith itself.
          shellHook = ''
            export CHIPSMITH_CYCLONE2_GRAPH=$PWD/target/routing-graph.bin
          '';
        };
      }
    );
}
