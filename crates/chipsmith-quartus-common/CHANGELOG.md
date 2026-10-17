# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/A-H-Technology/chipsmith/compare/chipsmith-quartus-common-v0.1.0...chipsmith-quartus-common-v0.2.0) - 2026-10-17

### Added

- `chipsmith build --out-dir` hands over the bitstream and timing report
- pin Nix Compat at build time and stamp what an install was patched against
- run testbenches with `chipsmith test`
- carry the timing verdict out of the build instead of printing it

### Fixed

- send Accept-Encoding and Accept-Language so Akamai serves installers
- keep glibc off LD_LIBRARY_PATH
- let Quartus write outside $HOME under the 32-bit sandbox
- make the Bubblewrap32 sandbox actually start Quartus II 13

### Other

- default the examples to GHDL
- put every subprocess and host probe behind a ProcessHost seam
- put the version space in the Toolchain interface, dissolve core
- give the build layout one owner and make planning pure
- parse the [toolchain] table into a type that cannot be wrong
- collapse the forked Quartus backends onto one adapter
