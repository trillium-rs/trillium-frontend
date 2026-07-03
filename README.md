# trillium-frontend

[![ci][ci-badge]][ci]
[![crates.io version badge][version-badge]][crate]

[ci]: https://github.com/trillium-rs/trillium-frontend/actions?query=workflow%3ACI
[ci-badge]: https://github.com/trillium-rs/trillium-frontend/workflows/CI/badge.svg
[version-badge]: https://img.shields.io/crates/v/trillium-frontend.svg?style=flat-square
[crate]: https://crates.io/crates/trillium-frontend

A [Trillium](https://trillium.rs) handler for serving JS frontend projects with three operating modes:

- **Build mode** (default, source available): runs the frontend build at **compile time** and embeds all dist assets directly in the binary via [`trillium-static-compiled`](https://docs.rs/trillium-static-compiled)
- **Prebuilt mode** (default, no source): embeds pre-built dist assets without attempting to build — this is what happens during `cargo install` from crates.io
- **Dev-proxy mode** (`dev-proxy` feature + source available): auto-detects your framework and package manager, spawns the dev server on a free port, and proxies all requests to it — including WebSocket upgrades for HMR

## Usage


```rust
use trillium_client::Client;
use trillium_frontend::frontend;
use trillium_smol::ClientConfig;

fn main() {
    trillium_smol::run((
        frontend!("./client")
            .with_client(Client::new(ClientConfig::default()))
            .with_index_file("index.html"),
    ));
}
```

The same code works in all three modes — `.with_client()` is accepted but ignored when the `dev-proxy` feature is not enabled.

## Mode selection

The mode is determined by two factors:

1. Whether the `dev-proxy` cargo feature is enabled
2. Whether `package.json` exists in the frontend project directory (i.e., source is available)

| `dev-proxy` feature | `package.json` exists | Mode |
|--------------------|-----------------------|------|
| enabled | yes | Dev-proxy |
| enabled | no | Prebuilt (fallback) |
| disabled | yes | Build |
| disabled | no | Prebuilt |

## Enabling dev-proxy mode

Pass the feature flag when you want live-reloading during development:

```sh
# Development: live-reloading proxy
cargo run --features trillium-frontend/dev-proxy

# Production: build and embed assets at compile time
cargo build --release
```

You can also define a feature in your own crate that forwards it, for brevity:

```toml
[features]
dev-proxy = ["trillium-frontend/dev-proxy"]
```

```sh
cargo run --features dev-proxy
```

## Macro syntax

```rust
// Simple: path is relative to the Cargo.toml of the calling crate
frontend!("./client")

// With explicit overrides
frontend!(
    path = "./client",
    build = "bun run build",
    dist = "dist",
)
```

| Argument | Description |
|----------|-------------|
| `path` | Path to the frontend project directory (required) |
| `build` | Build command override (default: auto-detected from framework) |
| `dist` | Dist directory name relative to `path` (default: auto-detected, usually `"dist"`) |

## Auto-detection

When source is available (`package.json` exists), `trillium-frontend` inspects the project directory for known config and lock files to determine the build and dev commands.

**Package manager** (detected by lock file, in priority order):

| Lock file | Package manager | Run prefix |
|-----------|----------------|------------|
| `bun.lockb` / `bun.lock` | Bun | `bun run` |
| `pnpm-lock.yaml` | pnpm | `pnpm exec` |
| `yarn.lock` | Yarn | `yarn exec` |
| `package-lock.json` | npm | `npx` |

**Framework** (detected by config file):

| Config file | Framework | Dev command | Build command | Dist dir |
|-------------|-----------|-------------|---------------|----------|
| `vite.config.{js,ts,mjs}` | Vite | `vite --strictPort --clearScreen false` | `vite build` | `dist` |
| `webpack.config.{js,ts,mjs}` | Webpack | `webpack serve` | `webpack build` | `dist` |
| `next.config.{js,ts,mjs}` | Next.js | `next dev` | `next build` | `.next` |

The full dev command is assembled as `{run_prefix} {framework_command} --port {port}`. The `PORT` environment variable is also set, so you can reference it in a custom `dev_command`.

## Builder API

`FrontendHandler` is returned by the `frontend!` macro and offers these builder methods:

| Method | Description |
|--------|-------------|
| `.with_client(Client)` | **Required in dev-proxy mode.** Provide your runtime's HTTP connector for the proxy. Accepted but ignored in other modes. |
| `.with_index_file("index.html")` | Enable SPA fallback: serve this file for any unmatched path. |
| `.with_dev_command("npm run dev -- --port $PORT")` | Override the auto-detected dev command. When set, you are responsible for port handling — `$PORT` is exported as an env var. Only used in dev-proxy mode. |
| `.with_dev_port(3000)` | Pin the dev server to a specific port instead of picking a free one automatically. Only used in dev-proxy mode. |

## Rebuilding when frontend sources change

In **build mode**, the frontend build runs at compile time, so cargo needs to know that your JS/TS sources are inputs to the compile — otherwise editing *only* a frontend file and re-running `cargo build` can leave the previously embedded assets in place.

- **Dev-proxy mode:** not a concern. A live dev server serves your sources with HMR, so nothing is embedded.
- **Build mode on nightly:** handled automatically. The macro registers your source files as compile dependencies (via the unstable [`proc_macro_tracked_path`](https://github.com/rust-lang/rust/issues/99515) feature, detected at build time — nothing to embed, no setup). Edit a source file, `cargo build`, and the frontend rebuilds.
- **Build mode on stable:** proc macros have no stable way to declare file dependencies, so a frontend-only edit may be missed until something else triggers a recompile (editing Rust code, `cargo clean`, etc.).

If you build or develop on stable and want frontend-only edits to reliably trigger a rebuild — or just want belt-and-suspenders insurance that you'll never ship stale assets, on any toolchain — add a one-line `build.rs`. It's optional, not essential:

```rust
// build.rs
fn main() {
    trillium_frontend::build::track_frontend_sources();
}
```

```toml
[build-dependencies]
trillium-frontend = "0.3"
```

The macro leaves a manifest of your source paths in `OUT_DIR`; this helper reads it and emits `cargo:rerun-if-changed` for each. It's safe to leave in place across toolchains: on nightly it runs alongside the macro's own tracking, declaring the same dependencies a second way. (That redundancy is deliberate — emitting explicit `rerun-if-changed` paths also keeps the build script from falling back to cargo's whole-package change detection, which can otherwise rebuild on every `cargo build`.) This shim exists only until `proc_macro_tracked_path` stabilizes. See the [`build` module docs](https://docs.rs/trillium-frontend/latest/trillium_frontend/build/) for caveats (notably: it reflects the *previous* build's file list, and the frontend project should live inside your crate directory).

## Publishing for `cargo install`

To support `cargo install` without requiring JS tooling on the user's machine, include the dist directory in your published crate but **exclude** `package.json`:

```toml
[package]
include = [
    "src/",
    "client/dist/",   # pre-built assets to embed
    # Do NOT include client/package.json — its absence triggers prebuilt mode
]
```

The absence of `package.json` tells `trillium-frontend` to use the pre-built assets as-is without attempting to run a build command.

Then publish with:

```sh
cargo build --release  # triggers the frontend build via the proc macro
cargo publish
```

## How it works

### Dev-proxy mode (`dev-proxy` feature + source)

1. The proc macro detects the framework and package manager at **compile time** and records the dev command in the `FrontendHandler`.
2. When Trillium calls `Handler::init`, `FrontendHandler`:
   - Picks a free port (or uses `.with_dev_port`)
   - Spawns `sh -c "<dev_command> --port <port>"` in the project directory
   - Polls the port until the dev server responds (up to ~1 second with 10 ms retries)
   - Builds a `trillium-proxy` pointing at `http://localhost:<port>` with WebSocket upgrade support
3. All subsequent requests (HTTP and WS) are forwarded to the dev server.
4. On `Drop`, the dev process is killed.

### Build mode (no `dev-proxy` feature + source)

1. At **compile time**, the proc macro runs the build command (`sh -c "<build_command>"`) in the project directory.
2. The compiled `dist/` directory is embedded using `trillium-static-compiled::static_compiled!`.
3. If `dist/index.html` exists, it is also embedded as a separate SPA fallback handler.
4. No external process is spawned at runtime.

### Prebuilt mode (no source)

1. At **compile time**, the proc macro finds no `package.json` and skips the build step entirely.
2. The existing `dist/` directory is embedded using `trillium-static-compiled::static_compiled!`.
3. Identical runtime behavior to build mode.

## Safety

This crate uses `#![forbid(unsafe_code)]` (via its dependencies; the crate itself contains no unsafe code).

## License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

---

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
