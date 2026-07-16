# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/trillium-rs/trillium-frontend/compare/trillium-frontend-macros-v0.4.0...trillium-frontend-macros-v0.4.1) - 2026-07-16

### Fixed

- serialize concurrent frontend! expansions

## [0.4.0](https://github.com/trillium-rs/trillium-frontend/compare/trillium-frontend-v0.3.0...trillium-frontend-v0.4.0) - 2026-07-03

### Added

- **Frontend source tracking in build mode.** Build mode compiles and embeds your frontend at
  compile time, but cargo had no idea your JS/TS files were inputs to that compile — so editing
  only a frontend file and rebuilding could quietly ship the previously embedded assets. That
  gap is now closed: a source edit re-runs the frontend build and re-embeds the output on the
  next `cargo build`, while builds with no source changes stay fully cached.

  - *Nightly* — automatic, zero setup. The macro declares your source files as compile
    dependencies through the unstable [`proc_macro_tracked_path`] feature, detected at build
    time. Nothing extra is embedded and no build script is required.
  - *Stable* — proc macros have no stable way to declare file dependencies, so opt in with a
    one-line `build.rs` calling `trillium_frontend::build::track_frontend_sources()` (from the
    new public `build` module) plus a `trillium-frontend` build-dependency. Safe to leave in
    place on any toolchain; it's a temporary shim until [`proc_macro_tracked_path`] stabilizes.

  Dev-proxy mode is unaffected — a live dev server with HMR serves your sources, so nothing is
  embedded.

[`proc_macro_tracked_path`]: https://github.com/rust-lang/rust/issues/99515


## [0.3.0] - 2026-06-30

### Other

- Release for trillium 1

## [0.3.0-rc.1](https://github.com/trillium-rs/trillium-frontend/compare/trillium-frontend-macros-v0.2.0...trillium-frontend-macros-v0.3.0-rc.1) - 2026-04-08

### Other

- release

## [0.2.0](https://github.com/trillium-rs/trillium-frontend/compare/trillium-frontend-macros-v0.1.0...trillium-frontend-macros-v0.2.0) - 2026-02-28

### Added

- install if needed
- [**breaking**] revisit how build mode is selected

### Other

- bump versions
- fmt
