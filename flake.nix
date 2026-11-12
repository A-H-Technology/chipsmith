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
    # 7-series: prjxray-db, which the build reads, and prjxray (fasm2frames,
    # xc7frames2bit, bit2fasm), fasm and openXC7's nextpnr, which only the
    # tests run, as oracles; all one consistent set. Pinned to 42323cb
    # (nextpnr 3e5c2cdd, prjxray-db 517d66a3). It keeps its own nixpkgs on
    # purpose: its packages (boost, python 3.12, antlr 4.9) are only
    # verified against that one. Bump with the rest of the set.
    openxc7.url = "github:openXC7/toolchain-nix/42323cb00c8b76486619695166498dfa72b1e3e2";
    # The Agilex 5 database and keys (its db package).
    agilex5-re = {
      url = "github:cramt/agilex5-re";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      cyclone2-re,
      openxc7,
      agilex5-re,
      ...
    }:
    let
      # The vendor-tool runner's NixOS VM tests (nix/vendor/tests).
      vendorTests = import ./nix/vendor/tests {
        inherit self;
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
      };
      vendorPrefixed = nixpkgs.lib.mapAttrs' (n: nixpkgs.lib.nameValuePair "vendor-${n}");
    in
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
          # Co-simulation in the tests (tests/support/cosim.rs).
          pkgs.iverilog
          xc7.tools
          # The 7-series flow chipsmith replaced, to compare against.
          xc7.nextpnr
        ];
        chipsmith = pkgs.callPackage ./nix/package.nix {
          cyclone2-db = open.db;
          agilex5-db = agilex5-re.packages.${system}.db;
          agilex5-graph = agilex5-re.packages.${system}.graph;
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
          inherit chipsmith;
          # The original chipsmith, which installs and runs the vendor tools
          # (Quartus) the open flow is tested and reverse-engineered against.
          chipsmith-vendor = pkgs.callPackage ./nix/vendor/package.nix { };
          xc7-chipdb = xc7.chipdb;
          xc7-routing-graph = chipsmith.xc7RoutingGraph;
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            # CI runs the tests with nextest (.github/workflows/ci.yml).
            pkgs.cargo-nextest
            pkgs.just
            pkgs.pkg-config
            pkgs.libusb1
            # CubeCL's HIP backend: build.rs reads the HIP version off
            # `hipconfig`, and at runtime it dlopens libamdhip64/libhiprtc
            # from $ROCM_PATH/lib.
            pkgs.rocmPackages.clr
            cudaToolkit
            # CubeCL builds (`--features gpu`, experiments/gpu-bench) link with
            # lld (crates/chipsmith/build.rs). nixpkgs' wrapped one, alone:
            # it adds the rpaths of the libraries it links (libusb), which
            # the bare `pkgs.lld` doesn't, and the rest of LLVM's binutils
            # would shadow GNU's for everything else.
            (pkgs.runCommand "wrapped-ld-lld" { } ''
              mkdir -p $out/bin
              ln -s ${pkgs.llvmPackages.bintools}/bin/ld.lld $out/bin/ld.lld
            '')
            # Random co-simulation of M4K designs against their RTL (tests).
            pkgs.iverilog
            # Assembles the Zynq first-stage loader (crates/chipsmith-xc7/src/
            # zynq_loader.S); its binary is committed, and a test checks it
            # against this.
            pkgs.pkgsCross.arm-embedded.buildPackages.binutils
          ]
          ++ flowTools;
          ROCM_PATH = pkgs.rocmPackages.clr;
          CUDA_PATH = cudaToolkit;
          # cyclone2-re's yosys flow, the oracle chipsmith's synthesis is
          # tested against (ADR-0004; tests, `de2_synth --yosys`). The
          # build doesn't read it.
          CHIPSMITH_CYCLONE2_SYNTH = open.cyclone2-synth;
          CHIPSMITH_CYCLONE2_DB = "${open.db}";
          CHIPSMITH_AGILEX5_DB = "${agilex5-re.packages.${system}.db}";
          CHIPSMITH_AGILEX5_GRAPH = "${agilex5-re.packages.${system}.graph}";
          # nextpnr's chip database, for comparing against the flow it ran.
          CHIPSMITH_XC7_CHIPDB = xc7.chipdb;
          CHIPSMITH_PRJXRAY_DB = xc7.prjxray-db;
          # Written on first use (and whenever the format changes), so the
          # shell doesn't depend on building chipsmith itself. Named after
          # the database it is flattened from: the cache only checks its
          # format, so a graph of an older database (one without the PLL
          # pins, say) would otherwise outlive a flake.lock bump.
          shellHook = ''
            export CHIPSMITH_CYCLONE2_GRAPH=$PWD/target/routing-graph-${builtins.baseNameOf open.db}.bin
            # `-2`: the import keeps the I/O and clock-row pseudo pips since
            # chipsmith routes everything itself (crates/chipsmith-xc7/src/xray.rs).
            export CHIPSMITH_XC7_GRAPH=$PWD/target/xc7-routing-graph-2-${builtins.baseNameOf xc7.prjxray-db}.bin
            # cudarc dlopens libcuda by bare name; NixOS puts the driver's
            # copy here rather than anywhere ld.so searches.
            export LD_LIBRARY_PATH=/run/opengl-driver/lib:${cudaToolkit}/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
          '';
        };
      }
    )
    // {
      nixosModules.chipsmith-vendor = import ./nix/vendor/module.nix { inherit self; };
      vendorLib = import ./nix/vendor/lib.nix { inherit self; };
      checks.x86_64-linux = vendorPrefixed vendorTests.checks;
      # e2e tests whose inputs nobody can fetch anonymously; see nix/vendor/tests
      legacyPackages.x86_64-linux = vendorPrefixed vendorTests.manual;
    };
}
