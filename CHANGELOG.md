# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

All packages share one version: the crates on crates.io (`qweb-core`,
`qweb-value`, `qweb-inherit`, `qweb-wasm`), the Python package on PyPI
(`qweb-rs`) and the npm package (`qweb-wasm`). Each release is tagged `vX.Y.Z`.

## [Unreleased]

## [0.2.0](https://github.com/DBosch0/qweb-rs/compare/v0.1.2...v0.2.0) - 2026-11-18

Faster rendering through the Python package: a cached `Engine.render` takes
roughly a third less time than 0.1.2 on a 500-row table and a fifth less on
nested `t-call`s. The output is unchanged.

### Changed (breaking)

- `qweb-core` IR: new `Stmt::Attrs`, `ir::Attr` and `ir::LoopVars`; new fields
  `Foreach::each`, `Foreach::live`, `Foreach::name` and `Out::attrs`.
- `LoopSource::size` is an `Option<i64>`: an int loop source is its own size,
  even negative (`x_size` is then that int, as in Python).
- `CompileOptions` and `CompileHooks` are `#[non_exhaustive]`: build them from
  `Default::default()` and set the fields, so that new options no longer need
  a breaking release.
- `qweb_value::ops::call_builtin` takes the arguments as a slice (`&[Value]`).

### Added

- `Host::write_str_escaped`, `Host::scope_share` and `Host::scope_is_unique`,
  with default implementations.
- `qweb_core::num`: Python's `str()` of ints and floats.
- `CompileHooks` is re-exported at the crate root.

### Performance

- Numbers are formatted in Rust instead of calling `str()`.
- Attributes set and rendered in a row (including on `t-out` elements) are
  written directly, without a temporary dict.
- `t-foreach` assigns only the loop variables that can be read.
- Lazy `t-set`/`t-call` bodies share the scope and copy it only on a write.
- `t-call` without `t-args` assigns its values straight into the new scope.

### Fixed

- A float exactly halfway between two shortest decimals is printed like
  Python's `repr` (ties to even) in `qweb-value`.
- Nested `t-foreach` loops share one `foreach_attrs`, as in Python.
- `t-foreach` over a negative int sets `x_size` to that int.

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
