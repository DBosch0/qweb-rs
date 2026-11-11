# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

All packages share one version: the crates on crates.io (`qweb-core`,
`qweb-value`, `qweb-inherit`, `qweb-wasm`), the Python package on PyPI
(`qweb-rs`) and the npm package (`qweb-wasm`). Each release is tagged `vX.Y.Z`.

## [Unreleased]

## [0.1.3](https://github.com/DBosch0/qweb-rs/compare/v0.1.2...v0.1.3) - 2026-11-11

### Other

- resolving pedantic lints
- add badges

## [0.1.2](https://github.com/DBosch0/qweb-rs/compare/v0.1.0...v0.1.2) - 2026-11-11

First release on all three registries.

### Added

- Python package `qweb-rs` on PyPI: abi3 wheels for Linux (manylinux and
  musllinux, x86_64 and aarch64), macOS (arm64 and x86_64) and Windows x64,
  plus the source distribution.
- npm package `qweb-wasm`: a `web` build for browsers and bundlers, and a
  synchronous `node` build for Node (`import` and `require`).
- Releases are automated with release-plz; crates.io, PyPI and npm publish
  with trusted publishing.

### Changed

- Every crate ships the `LICENSE` and `COPYRIGHT` files.
- Minimum supported Rust version declared: 1.88.0.
- The internal dependencies carry a version, as crates.io requires.

## 0.1.1 - 2026-11-11

Published on npm only (`qweb-wasm`), to set up npm publishing. Not released
on crates.io or PyPI.

## [0.1.0](https://github.com/DBosch0/qweb-rs/releases/tag/v0.1.0) - 2026-11-11

Initial release of `qweb-core`, `qweb-value`, `qweb-inherit` and `qweb-wasm`
on crates.io.
