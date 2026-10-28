{
  description = "chipsmith: an open FPGA toolchain, HDL in, programmed chip out";

  inputs = {
    # The Cyclone II device and chip databases (its db package), and the
    # yosys techmap the tests compare chipsmith's synthesis against.
    cyclone2-re.url = "github:cramt/cyclone2-re";
    # One nixpkgs for both, so the tools are the same derivations
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
          xc7.tools
        ];
        chipsmith = pkgs.callPackage ./nix/package.nix {
          cyclone2-db = open.db;
          inherit xc7;
        };
        # CUDA is unfree; this instance exists only so the rest of the
        # flake keeps nixpkgs' default refusal of unfree packages.
        cudaPackages =
          (import nixpkgs {
            inherit system;
            config.allowUnfree = true;
          }).cudaPackages;
        # What CubeCL's CUDA backend reads: cudarc's build.rs takes the CUDA
        # version from `nvcc --version`, cubecl-cuda hands NVRTC
        # $CUDA_PATH/include, and NVRTC is dlopened at runtime.
        cudaToolkit = pkgs.symlinkJoin {
          name = "cubecl-cuda-toolkit";
          paths = with cudaPackages; [
            cuda_nvcc
            (pkgs.lib.getDev cuda_cudart)
            (pkgs.lib.getDev cuda_nvrtc)
            (pkgs.lib.getLib cuda_nvrtc)
            cccl
          ];
        };
      in
      {
        packages = {
          default = chipsmith;
          xc7-chipdb = xc7.chipdb;
          xc7-routing-graph = chipsmith.xc7RoutingGraph;
          cubecl-hello = pkgs.callPackage ./nix/cubecl-hello.nix { inherit cudaToolkit; };
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
            # CubeCL's HIP backend: build.rs reads the HIP version off
            # `hipconfig`, and at runtime it dlopens libamdhip64/libhiprtc
            # from $ROCM_PATH/lib.
            pkgs.rocmPackages.clr
            cudaToolkit
            # experiments/cubecl-hello links with it, see its .cargo/config.toml.
            pkgs.lld
          ]
          ++ flowTools;
          ROCM_PATH = pkgs.rocmPackages.clr;
          CUDA_PATH = cudaToolkit;
          # cyclone2-re's yosys flow, the oracle chipsmith's synthesis is
          # tested against (ADR-0004; tests, `de2_synth --yosys`). The
          # build doesn't read it.
          CHIPSMITH_CYCLONE2_SYNTH = open.cyclone2-synth;
          CHIPSMITH_CYCLONE2_DB = "${open.db}";
          CHIPSMITH_XC7_CHIPDB = xc7.chipdb;
          CHIPSMITH_PRJXRAY_DB = xc7.prjxray-db;
          # Written on first use (and whenever the format changes), so the
          # shell doesn't depend on building chipsmith itself.
          shellHook = ''
            export CHIPSMITH_CYCLONE2_GRAPH=$PWD/target/routing-graph.bin
            export CHIPSMITH_XC7_GRAPH=$PWD/target/xc7-routing-graph.bin
            # cudarc dlopens libcuda by bare name; NixOS puts the driver's
            # copy here rather than anywhere ld.so searches.
            export LD_LIBRARY_PATH=/run/opengl-driver/lib:${cudaToolkit}/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
          '';
        };
      }
    );
}
