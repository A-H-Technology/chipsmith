# Multipliers branch: status and handoff (2026-10-29)

Branch `multipliers` here and in cyclone2-re (`/pool/dev/cyclone2-re-wt/mult`).
Design: ADR-0010. Not merged into `open`. The README has no multiplier
section yet; write one once the numbers below are complete.

## Done and verified

- **Quartus byte-identical** (`bench/quartus-validate.sh`, cyclone2-re
  f0406b4): `fir` and `mult-selftest`. Two things were in the way, and
  neither was a multiplier problem:
  - a carry chain whose low cells had a forced carry (`step <= 64`). These
    cells did nothing, and quartus_map sweeps them out of a VQM.
    `synth/arith.rs` now builds no LE for them (mult-selftest went from
    1014 to 1008 LEs).
  - inverting feeders (a register powering up 1 fed from a register or a
    pin). Quartus makes these `<src>~_wirecell` and places them in some
    free LE. The cyclone2-re export now names the feeder that way and
    places it in the feeder's LE: fir went from 100 dropped rcf routes to
    0, mult-selftest from about 40 to 0.
- **Board** (DE2, chipsmith 41afcdf, loaded over JTAG with
  `chipsmith -C examples/mult-selftest flash`): `"MUL!"`, 43,410
  passes/s (expected 50 MHz / 1152 = 43,403), 0 errors in 6 reads over
  about 60 s (about 2.6 M passes). Bit 0 of the error word can't be read.
- **Tests passed after the arith change** (`cargo test --release -p
  chipsmith-cyclone2 --test synth mult`, stopped part-way):
  - proofs: const_k (64 $equiv, 10.5 s), one_bit (52, 2.1 s), reg_in
    (72, 632 s), reg_in_out (72, 230 s), reg_out (54, 1.4 s),
    reg_in_shared (63, 474 s), reg_out_other_enable (72, 216 s).
  - co-simulation: reg_enable_clear, plus dense u20x20, s24x24, u32x32_32,
    s32x32 and u36x18 (5000 cycles each).
  - mutation check: a swapped operand bit is caught at cycle 18 and a
    wrong sign at cycle 4.
  - `mult_registers_move_in_only_where_legal`.
- **Chip database**: `cram chipdb` output is byte-equal to the committed
  `db/EP2C35F672C6/chipdb.txt.xz`.

## Built but not verified

- The other per-shape proofs after the arith change were not re-run:
  u9x9, s9x9, s9x3, u4x7, u18x18, s18x18, s12x7, u18x10, u16x16_16, the
  five `*_sparse` shapes, s12xu8 and two_products.
- New tests in `tests/synth.rs` that have **not been compiled or run**:
  - `mult_fir`: proof plus co-simulation.
  - `mult_selftest`: 120,000 co-simulated cycles.
  - `mult_selftest_catches_a_wrong_sign`.

  They need the `CY2_JTAG` stub that was added to `tests/support/mod.rs`
  (also unrun).
- RBF changes from the arith change: built the old commit (c4c693e) and
  the new one side by side. Of the examples checked, only i2c-master's RBF
  changed. bidir, blinky-*, de2-jtag-counter, de2-pll-counter, femtorv32,
  m4k-* (all), picorv32, ram, ram-leds and ram_widths came out the same.
  serv, sha256, spi-master and uart were not compared. i2c-master was not
  byte-identical with Quartus before this change; it hasn't been
  re-validated since.
- The wirecell export may also fix uart and i2c-master (an inverter from
  a pin into a register, README). Not checked.

## Not started

- The full `--test synth` suite after the arith change, which affects
  every design.
- `cyclone2_re_specimens` (`crates/chipsmith-verilog/tests/yosys.rs`, with
  `CYCLONE2_RE=/pool/dev/cyclone2-re-wt/mult`). The mult specimens
  instantiate `cycloneii_mac_mult`/`_mac_out`; `device_cells.v` has
  blackboxes for them. Not run.
- README sections and numbers, in both repos.
- The `small-gaps` branch: negedge clocks, arrays declared inside
  functions, and crc32 back in the corpus (removed in a3e3dfe).
- Switching cyclone2-re's chipsmith dependency from the worktree path to
  a git rev (Cargo.toml, both lockfiles, `nix/cram.nix` outputHash).

## Next step

Compile and run the three new tests, and finish the mult shape proofs.

## Commands

Development environment (chip database from the cyclone2-re worktree, its
own graph cache):

```
cat > /tmp/csenv.sh <<'EOF'
export CHIPSMITH_CYCLONE2_DB=/pool/dev/cyclone2-re-wt/mult/db
export CHIPSMITH_CYCLONE2_GRAPH=/pool/dev/chipsmith-wt/mult/target/routing-graph-mult.bin
export CYCLONE2_RE=/pool/dev/cyclone2-re-wt/mult
EOF
cd /pool/dev/chipsmith-wt/mult
S="systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0"
$S nix develop --command bash -c 'source /tmp/csenv.sh; cargo test --release -p chipsmith-cyclone2 --test synth mult -- --test-threads=2 --nocapture'
$S nix develop --command bash -c 'source /tmp/csenv.sh; cargo build --release -p chipsmith && target/release/chipsmith -C examples/fir build'
$S nix develop --command bash -c 'source /tmp/csenv.sh; bench/quartus-validate.sh examples/fir examples/mult-selftest'
nix develop --command bash -c 'source /tmp/csenv.sh; target/release/chipsmith -C examples/mult-selftest flash'
nix develop --command cargo run -q --release -p chipsmith-cyclone2 --example de2_user1
$S nix develop --command bash -c 'source /tmp/csenv.sh; cargo test --release -p chipsmith-verilog --test yosys cyclone2_re_specimens -- --nocapture'
```

Before `quartus-validate`, build cram: `cargo build --release --manifest-path
tools/cram/Cargo.toml` in the cyclone2-re worktree. The
`/pool/dev/chipsmith-wt/mult-base` worktree (c4c693e) was only for the RBF
comparison and can be removed.
