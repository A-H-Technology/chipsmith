{
  description = "chipsmith: an open FPGA toolchain, HDL in, programmed chip out";

  inputs = {
    # The Cyclone II device database (cram's db/), techmap and cram.
    cyclone2-re.url = "github:cramt/cyclone2-re";
    # One nixpkgs for both, so cram and its tools are the same derivations
    # cyclone2-re's devshell already built.
    nixpkgs.follows = "cyclone2-re/nixpkgs";
    flake-utils.follows = "cyclone2-re/flake-utils";
    # 7-series: openXC7's nextpnr (himbaechel xilinx), prjxray (fasm2frames,
    # xc7frames2bit), fasm and their prjxray-db, all one consistent set.
    # Pinned to 42323cb (nextpnr 3e5c2cdd, prjxray-db 517d66a3): the first
    # revision whose nextpnr places BSCANE2 (openXC7/nextpnr#29), which
    # examples/zybo-jtag-counter needs. It keeps its own nixpkgs on purpose:
    # its packages (boost, python 3.12, antlr 4.9) are only verified against
    # that one. Bump with the rest of the set; drop once chipsmith reads
    # prjxray-db itself and writes its own bitstream.
    openxc7.url = "github:openXC7/toolchain-nix/42323cb00c8b76486619695166498dfa72b1e3e2";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      cyclone2-re,
      openxc7,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        open = cyclone2-re.packages.${system};
        xc7 = pkgs.callPackage ./nix/xc7.nix {
          xpkgs = openxc7.inputs.nixpkgs.legacyPackages.${system};
          openxc7 = openxc7.packages.${system};
        };
        flowTools = [
          pkgs.ghdl
          pkgs.yosys
          open.cram
          xc7.tools
        ];
        chipsmith = pkgs.callPackage ./nix/package.nix {
          inherit (open)
            cram
            cyclone2-synth
            ;
          inherit xc7;
        };
      in
      {
        packages = {
          default = chipsmith;
          xc7-chipdb = xc7.chipdb;
          xc7-routing-graph = chipsmith.xc7RoutingGraph;
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
          CHIPSMITH_CYCLONE2_DB = "${open.cram}/db";
          CHIPSMITH_XC7_CHIPDB = xc7.chipdb;
          CHIPSMITH_PRJXRAY_DB = xc7.prjxray-db;
          # Written on first use (and whenever the format changes), so the
          # shell doesn't depend on building chipsmith itself.
          shellHook = ''
            export CHIPSMITH_CYCLONE2_GRAPH=$PWD/target/routing-graph.bin
            export CHIPSMITH_XC7_GRAPH=$PWD/target/xc7-routing-graph.bin
          '';
        };
      }
    );
}
