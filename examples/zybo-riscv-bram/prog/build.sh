#!/usr/bin/env bash
# Assembles selftest.S into ../src/prog.hex (one 32-bit word a line, for
# $readmemh), with nixpkgs' riscv32-none-elf binutils. The build reads
# the checked-in prog.hex and needs no RISC-V tools.
set -euo pipefail
cd "$(dirname "$0")"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
nix shell nixpkgs#pkgsCross.riscv32-embedded.buildPackages.binutils --command bash -c "
  riscv32-none-elf-as -march=rv32i -o $tmp/p.o selftest.S &&
  riscv32-none-elf-ld -Ttext=0 -o $tmp/p.elf $tmp/p.o &&
  riscv32-none-elf-objcopy -O binary $tmp/p.elf $tmp/p.bin"
od -An -v -tx4 -w4 "$tmp/p.bin" | tr -d ' ' > ../src/prog.hex
