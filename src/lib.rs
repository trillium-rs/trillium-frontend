//! A [Trillium](https://trillium.rs) handler for serving JS frontend projects with
//! three operating modes:
//!
//! - **Build mode** (default, source available): runs the frontend build at **compile time** and
//!   embeds all dist assets directly in the binary via [`trillium_static_compiled`]
//! - **Prebuilt mode** (default, no source): embeds pre-built dist assets without attempting to
//!   build — this is what happens during `cargo install` from crates.io when the published crate
//!   includes only the dist directory
//! - **Dev-proxy mode** (`dev-proxy` feature + source available): auto-detects your framework and
//!   package manager, spawns the dev server on a free port, and proxies all requests to it —
//!   including WebSocket upgrades for HMR
//!
//! # Mode selection
//!
//! The mode is determined by two factors:
//!
//! 1. Whether the `dev-proxy` cargo feature is enabled
//! 2. Whether `package.json` exists in the frontend project directory (i.e., source is available)
//!
//! | `dev-proxy` feature | `package.json` exists | Mode |
//! |--------------------|-----------------------|------|
//! | enabled | yes | Dev-proxy |
//! | enabled | no | Prebuilt (fallback) |
//! | disabled | yes | Build |
//! | disabled | no | Prebuilt |
//!
//! # Usage
//!
//! ```rust,ignore
//! use trillium_client::Client;
//! use trillium_frontend::frontend;
//! use trillium_smol::ClientConfig;
//!
//! fn main() {
//!     trillium_smol::run((
//!         frontend!("./client")
//!             .with_client(Client::new(ClientConfig::default()))
//!             .with_index_file("index.html"),
//!     ));
//! }
//! ```
//!
//! The same code works in all three modes — `.with_client()` is accepted but ignored
//! when the `dev-proxy` feature is not enabled.
//!
//! # Enabling dev-proxy mode
//!
//! Pass the feature flag when you want live-reloading during development:
//!
//! ```sh
//! # Development: live-reloading proxy
//! cargo run --features trillium-frontend/dev-proxy
//!
//! # Production: build and embed assets at compile time
//! cargo build --release
//! ```
//!
//! You can also define a feature in your own crate that forwards it, for brevity:
//!
//! ```toml
//! [features]
//! dev-proxy = ["trillium-frontend/dev-proxy"]
//! ```
//!
//! ```sh
//! cargo run --features dev-proxy
//! ```
//!
//! # Macro syntax
//!
//! ```rust,ignore
//! // Simple: path is relative to the Cargo.toml of the calling crate
//! frontend!("./client")
//!
//! // With explicit overrides
//! frontend!(
//!     path = "./client",
//!     build = "bun run build",
//!     dist = "dist",
//! )
//! ```
//!
//! | Argument | Description |
//! |----------|-------------|
//! | `path`   | Path to the frontend project directory (required) |
//! | `build`  | Build command override (default: auto-detected from framework) |
//! | `dist`   | Dist directory name relative to `path` (default: auto-detected, usually `"dist"`) |
//!
//! # Auto-detection
//!
//! When source is available (`package.json` exists), `trillium-frontend` inspects the project
//! directory for known config and lock files to determine the build and dev commands.
//!
//! **Package manager** (detected by lock file, in priority order):
//!
//! | Lock file | Package manager | Run prefix |
//! |-----------|----------------|------------|
//! | `bun.lockb` / `bun.lock` | Bun | `bun run` |
//! | `pnpm-lock.yaml` | pnpm | `pnpm exec` |
//! | `yarn.lock` | Yarn | `yarn exec` |
//! | `package-lock.json` | npm | `npx` |
//!
//! **Framework** (detected by config file):
//!
//! | Config file | Framework | Dev command | Build command | Dist dir |
//! |-------------|-----------|-------------|---------------|----------|
//! | `vite.config.{js,ts,mjs}` | Vite | `vite --strictPort --clearScreen false` | `vite build` | `dist` |
//! | `webpack.config.{js,ts,mjs}` | Webpack | `webpack serve` | `webpack build` | `dist` |
//! | `next.config.{js,ts,mjs}` | Next.js | `next dev` | `next build` | `.next` |
//!
//! # Publishing for `cargo install`
//!
//! To support `cargo install` without requiring JS tooling on the user's machine:
//!
//! 1. Build your frontend assets (e.g., `cd client && npm run build`)
//! 2. Include the dist directory in your published crate, but **exclude** `package.json`
//!
//! The absence of `package.json` tells `trillium-frontend` to use the pre-built assets
//! as-is without attempting to run a build command.
//!
//! ```toml
//! [package]
//! include = [
//!     "src/",
//!     "client/dist/",   # pre-built assets to embed
//!     # Do NOT include client/package.json — its absence triggers prebuilt mode
//! ]
//! ```
//!
//! Then publish with:
//!
//! ```sh
//! cargo build --release  # triggers the frontend build via the proc macro
//! cargo publish
//! ```

#![forbid(unsafe_code)]

use fieldwork::Fieldwork;
use std::borrow::Cow;
use trillium::{Conn, Handler, Info};
use trillium_client::Client;
use trillium_static_compiled::StaticCompiledHandler;

#[cfg(feature = "dev-proxy")]
use std::{
    io::ErrorKind,
    process::{Child, Command},
    sync::Mutex,
    time::Duration,
};
#[cfg(feature = "dev-proxy")]
use trillium::{Error, Method, Upgrade};
#[cfg(feature = "dev-proxy")]
use trillium_proxy::{Proxy, Url};

#[derive(Fieldwork)]
#[fieldwork(opt_in, with, into, option_set_some)]
pub struct FrontendHandler {
    assets: Option<StaticCompiledHandler>,

    /// A `StaticCompiledHandler` pointing at the dist index file, used for SPA
    /// fallback (serves index for any path not matched by `assets`).
    spa_fallback: Option<StaticCompiledHandler>,

    #[allow(dead_code)]
    project_path: Cow<'static, str>,

    #[allow(dead_code)]
    detected_dev_command: Option<Cow<'static, str>>,

    /// trillium HTTP client for dev-mode proxying (ignored without `dev-proxy` feature)
    #[allow(dead_code)]
    #[field]
    client: Option<Client>,

    /// override auto-detected dev command (ignored without `dev-proxy` feature)
    #[allow(dead_code)]
    #[field]
    dev_command: Option<Cow<'static, str>>,

    /// override auto-detected dev port (ignored without `dev-proxy` feature)
    #[allow(dead_code)]
    #[field(into = false)]
    dev_port: Option<u16>,

    /// opt-in SPA index file fallback (e.g. `"index.html"`)
    #[field]
    index_file: Option<&'static str>,

    #[cfg(feature = "dev-proxy")]
    dev_process: Option<Mutex<Child>>,

    #[cfg(feature = "dev-proxy")]
    proxy: Option<Proxy<Url>>,
}

impl FrontendHandler {
    #[doc(hidden)]
    pub fn new(
        assets: Option<StaticCompiledHandler>,
        spa_fallback: Option<StaticCompiledHandler>,
        project_path: &'static str,
        detected_dev_command: Option<&'static str>,
    ) -> Self {
        FrontendHandler {
            assets,
            spa_fallback,
            project_path: Cow::Borrowed(project_path),
            detected_dev_command: detected_dev_command.map(Cow::Borrowed),
            client: None,
            dev_command: None,
            dev_port: None,
            index_file: None,
            #[cfg(feature = "dev-proxy")]
            dev_process: None,
            #[cfg(feature = "dev-proxy")]
            proxy: None,
        }
    }
}

impl Handler for FrontendHandler {
    async fn run(&self, conn: Conn) -> Conn {
        if let Some(assets) = self.assets {
            let conn = assets.run(conn).await;
            if !conn.is_halted()
                && let Some(spa) = self.spa_fallback.as_ref()
            {
                return spa.run(conn).await;
            }
            return conn;
        }

        #[cfg(feature = "dev-proxy")]
        if let Some(proxy) = &self.proxy {
            return proxy.run(conn).await;
        }

        conn
    }

    async fn init(&mut self, #[allow(unused)] info: &mut Info) {
        if self.assets.is_some() {
            if let Some(index) = self.index_file {
                self.assets = self.assets.map(|h| h.with_index_file(index));
            }
            return;
        }

        #[cfg(feature = "dev-proxy")]
        {
            let client = self.client.take().expect(
                "trillium-frontend: in dev-proxy mode, provide a Client via .with_client()",
            );

            let port = self.dev_port.unwrap_or_else(|| {
                portpicker::pick_unused_port()
                    .expect("trillium-frontend: could not find a free port for the dev server")
            });

            let dev_command = if let Some(cmd) = self.dev_command.as_deref() {
                cmd.to_string()
            } else {
                let detected = self.detected_dev_command.as_deref().expect(
                    "trillium-frontend: no dev command detected; configure with .with_dev_command()",
                );
                format!("{detected} --port {port}")
            };

            let child = Command::new("sh")
                .arg("-c")
                .arg(&dev_command)
                .env("PORT", port.to_string())
                .current_dir(self.project_path.as_ref())
                .spawn()
                .expect("trillium-frontend: failed to spawn dev server");
            self.dev_process = Some(Mutex::new(child));

            let upstream = format!("http://localhost:{port}").parse().unwrap();
            wait_for_port(&upstream, &client).await;

            let mut proxy = Proxy::new(client, upstream).with_websocket_upgrades();
            proxy.init(info).await;
            self.proxy = Some(proxy);
        }
    }

    #[cfg(feature = "dev-proxy")]
    fn has_upgrade(&self, upgrade: &Upgrade) -> bool {
        self.proxy.as_ref().is_some_and(|p| p.has_upgrade(upgrade))
    }

    #[cfg(feature = "dev-proxy")]
    async fn upgrade(&self, upgrade: Upgrade) {
        if let Some(proxy) = &self.proxy {
            proxy.upgrade(upgrade).await;
        }
    }
}

#[cfg(feature = "dev-proxy")]
impl Drop for FrontendHandler {
    fn drop(&mut self) {
        if let Some(mutex) = self.dev_process.take()
            && let Ok(mut child) = mutex.into_inner()
        {
            let _ = child.kill();
        }
    }
}

#[cfg(feature = "dev-proxy")]
async fn wait_for_port(upstream: &Url, client: &Client) {
    for _ in 0..100 {
        match client.build_conn(Method::Head, upstream.clone()).await {
            Ok(_) => {
                log::debug!("Successfully connected to {upstream}");
                // we don't care what the response was as long as it was valid http
                return;
            }

            Err(Error::Io(e)) if e.kind() == ErrorKind::ConnectionRefused => {
                // note(jbr): this is bad and represents a flaw in the current Connector api in that there's no
                // way to agnostically and asynchronously sleep
                std::thread::sleep(Duration::from_millis(10));
                log::debug!("Could not connect to {upstream} yet, sleeping 10ms");
            }

            Err(other) => {
                panic!("trillium-frontend was unable to connect to the dev server: {other}");
            }
        }
    }
}

#[doc(hidden)]
pub mod __macro_internals {
    pub use crate::FrontendHandler;
    pub use trillium_frontend_macros::frontend_impl;
    pub use trillium_static_compiled::static_compiled;
}

/// Build a [`FrontendHandler`] for your frontend project.
///
/// The mode is determined by the `dev-proxy` cargo feature and whether source
/// files (`package.json`) exist at the project path:
///
/// - **Dev-proxy** (`dev-proxy` feature + source): spawns dev server, proxies requests
/// - **Build** (no feature + source): builds frontend at compile time, embeds assets
/// - **Prebuilt** (no source): embeds pre-built dist assets as-is
///
/// # Usage
///
/// ```rust,ignore
/// // Simple: just the project path
/// trillium_frontend::frontend!("./client")
///     .with_client(Client::new(SmolConnector::default()))
///     .with_index_file("index.html")
///
/// // With explicit overrides
/// trillium_frontend::frontend!(
///     path = "./client",
///     build = "vite build",
///     dist = "dist",
/// )
/// ```
#[macro_export]
macro_rules! frontend {
    ($($tt:tt)*) => {{
        use $crate::__macro_internals::{FrontendHandler, static_compiled};
        $crate::__macro_internals::frontend_impl!($($tt)*)
    }};
}
