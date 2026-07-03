//! Build-script helper for tracking frontend sources on **stable** Rust.
//!
//! # Temporary shim
//!
//! On nightly, `trillium-frontend` registers your frontend source files as compile-time
//! dependencies automatically (via the unstable [`proc_macro_tracked_path`] feature), so
//! editing a source file re-runs the frontend build with no extra setup. Proc macros have
//! no *stable* equivalent for declaring file dependencies, so on stable the macro alone
//! cannot tell cargo it depends on those files — meaning source edits can go unnoticed and
//! the embedded assets go stale.
//!
//! Until [`proc_macro_tracked_path`] stabilizes, this shim bridges the gap. Add a
//! `build.rs` to the crate that calls `frontend!`:
//!
//! ```ignore
//! // build.rs
//! fn main() {
//!     trillium_frontend::build::track_frontend_sources();
//! }
//! ```
//!
//! and add `trillium-frontend` to your `[build-dependencies]`. It reads the manifest the
//! macro leaves in `OUT_DIR` and emits `cargo::rerun-if-changed` for each source path. It is
//! safe to leave in place across toolchains: on nightly it runs alongside the macro's own
//! `proc_macro::tracked::path` registration, harmlessly declaring the same dependencies a
//! second way.
//!
//! Emitting those paths is not merely redundant on nightly — it is what keeps the build
//! script from falling back to cargo's default "rerun if any package file changed"
//! detection. That fallback keys off the package's max source mtime (which, depending on
//! layout and VCS state, can include the freshly-rewritten `dist/`), and would rebuild on
//! every `cargo build`. Declaring the real source paths pins the build script to files the
//! build does *not* touch, so no-op builds stay cached.
//!
//! This entire module is expected to be removed once the nightly path stabilizes.
//!
//! ## Caveats
//!
//! - The manifest is written *during* compilation, which happens **after** build scripts
//!   run, so this shim always reflects the *previous* build's file list. In steady state
//!   that's fine; right after adding the shim — or after adding/removing source files that
//!   nothing else references — you may need one extra `cargo build` for the list to catch
//!   up.
//! - The frontend project must live inside your crate directory (the usual `./client`
//!   layout) for the very first build to notice later edits, since cargo's default
//!   build-script watching only covers files within the package until the manifest exists.
//!
//! [`proc_macro_tracked_path`]: https://github.com/rust-lang/rust/issues/99515

use std::{env, fs};

// These names must stay in sync with the macro (`trillium-frontend-macros`), which writes
// the manifests into the shared `OUT_DIR`.
const MANIFEST_PREFIX: &str = "trillium-frontend-";
const MANIFEST_SUFFIX: &str = ".paths";

/// Emit `cargo::rerun-if-changed` for every frontend source path the `frontend!` macro
/// recorded on the previous build.
///
/// Call this from your crate's `build.rs`. It emits nothing until the macro has written a
/// manifest (the very first build), after which it declares the recorded source paths on
/// every toolchain — including nightly, where it deliberately runs alongside the macro's own
/// tracking (see the [module docs](self) for why that redundancy matters). See the module
/// docs for setup and caveats.
pub fn track_frontend_sources() {
    for path in rerun_paths(env::var("OUT_DIR").ok().as_deref()) {
        println!("cargo::rerun-if-changed={path}");
    }
}

/// Compute the paths to emit as `rerun-if-changed` by reading the manifest(s) the macro
/// left in `out_dir`. Returns empty when `out_dir` is absent or no manifest exists yet.
fn rerun_paths(out_dir: Option<&str>) -> Vec<String> {
    let Some(out_dir) = out_dir else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(out_dir) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(MANIFEST_PREFIX) && name.ends_with(MANIFEST_SUFFIX)) {
            continue;
        }
        if let Ok(contents) = fs::read_to_string(entry.path()) {
            paths.extend(
                contents
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(String::from),
            );
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A throwaway directory standing in for cargo's `OUT_DIR`, cleaned up on drop.
    struct FakeOutDir(PathBuf);
    impl FakeOutDir {
        fn new(tag: &str) -> Self {
            let dir = env::temp_dir().join(format!("tf-shim-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            FakeOutDir(dir)
        }
        fn write(&self, name: &str, contents: &str) {
            fs::write(self.0.join(name), contents).unwrap();
        }
        fn as_str(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }
    impl Drop for FakeOutDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn no_out_dir_yields_nothing() {
        assert!(rerun_paths(None).is_empty());
    }

    #[test]
    fn empty_out_dir_yields_nothing() {
        let out = FakeOutDir::new("empty");
        assert!(rerun_paths(Some(out.as_str())).is_empty());
    }

    #[test]
    fn reads_manifest_paths() {
        let out = FakeOutDir::new("manifest");
        out.write(
            "trillium-frontend-00ff.paths",
            "/client/src\n/client/src/main.js\n",
        );
        let mut got = rerun_paths(Some(out.as_str()));
        got.sort();
        assert_eq!(got, ["/client/src", "/client/src/main.js"]);
    }

    #[test]
    fn unions_multiple_manifests_and_skips_blank_lines() {
        let out = FakeOutDir::new("multi");
        out.write("trillium-frontend-aaaa.paths", "/a/one\n\n/a/two\n");
        out.write("trillium-frontend-bbbb.paths", "/b/three\n");
        out.write("unrelated.txt", "/should/not/appear\n");
        let mut got = rerun_paths(Some(out.as_str()));
        got.sort();
        assert_eq!(got, ["/a/one", "/a/two", "/b/three"]);
    }

    /// The shim emits the manifest paths unconditionally — including on nightly, where the
    /// macro *also* tracks natively. Emitting them (rather than standing down) is what keeps
    /// the build script pinned to real source files instead of cargo's package-mtime
    /// fallback, which is prone to a rebuild-every-time loop when `dist/` is in the package
    /// tree. A stray file the macro happens to drop in `OUT_DIR` must not suppress that.
    #[test]
    fn emits_manifest_paths_regardless_of_other_out_dir_files() {
        let out = FakeOutDir::new("coexist");
        out.write("trillium-frontend-aaaa.paths", "/client/src\n");
        out.write("trillium-frontend.tracked", "");
        assert_eq!(
            rerun_paths(Some(out.as_str())),
            ["/client/src"],
            "the shim must always declare the recorded source paths"
        );
    }
}
