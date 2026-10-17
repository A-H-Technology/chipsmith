# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/A-H-Technology/chipsmith/compare/chipsmith-cli-v0.1.0...chipsmith-cli-v0.2.0) - 2026-10-17

### Added

- `chipsmith build --out-dir` hands over the bitstream and timing report
- run testbenches with `chipsmith test`
- carry the timing verdict out of the build instead of printing it

### Other

- put the version space in the Toolchain interface, dissolve core
