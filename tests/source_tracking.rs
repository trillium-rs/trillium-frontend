//! Hermetic integration tests for compile-time frontend **source tracking**.
//!
//! # What these assert (and, deliberately, what they don't)
//!
//! They assert the *observable end consequence* the feature exists to guarantee:
//!
//! 1. **Anti-stale:** after editing a frontend source file and rebuilding, the
//!    freshly built assets are the ones embedded in the binary.
//! 2. **No over-build:** a `cargo build` with no changes does *not* re-run the
//!    frontend build.
//! 3. **Build once:** compilation units expanding one `frontend!` concurrently
//!    produce exactly one frontend build, never a pile-up racing over one `dist`.
//!
//! The three interact, which is why they live together: the skip that keeps (3) from
//! double-building is the same skip that could, done wrong, embed stale assets and
//! break (1).
//!
//! They do **not** assert *how* that happens. The same assertions run under both
//! stable and nightly; each toolchain exercises a different mechanism underneath
//! — the nightly `proc_macro_tracked_path` path, or the stable `build.rs` shim
//! (`trillium_frontend::build::track_frontend_sources`). That's the whole point:
//! the properties are what users care about, and they must hold regardless of
//! which mechanism is active, so an orthogonal refactor that quietly breaks one
//! mechanism gets caught here.
//!
//! # Why they're hermetic
//!
//! The "frontend build" is a trivial shell command that copies a source file
//! into `dist/` — no JS toolchain, no Vite, no network beyond fetching Rust deps
//! on a cold cache. A unique token is written into that source file; the tests
//! then scan the **linked binary** for the token. Finding it proves the freshly
//! built assets were embedded (a stale build would embed the previous token).
//! That is the true end consequence — closer to "what a user's server serves"
//! than checking `dist/` on disk, which would only prove the build script ran.
//!
//! The fixture is a real (temp) git repo that gitignores `dist/`, mirroring a
//! real project. That matters: the no-over-build property on the nightly +
//! `build.rs` "belt and suspenders" path relies on the build output being
//! ignored (otherwise cargo's default whole-package change detection would see
//! `dist/` churn on every build and rebuild forever). Encoding that assumption
//! in the fixture is intentional — if it ever stops holding, these tests fail.
//!
//! # Running
//!
//! They're `#[ignore]`d because each runs a real `cargo build` of a generated
//! crate (slow, and needs the relevant toolchain installed). A scenario whose
//! toolchain isn't installed skips itself rather than failing.
//!
//! ```sh
//! cargo test --test source_tracking -- --ignored
//! # or a single scenario:
//! cargo test --test source_tracking -- --ignored stable_shim
//! ```

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::SystemTime,
};

/// Path to the `trillium-frontend` crate under test, used as a `path` dependency
/// in each generated fixture.
const CRATE_ROOT: &str = env!("CARGO_MANIFEST_DIR");

// ── Toolchain detection ─────────────────────────────────────────────────────

/// True if `cargo +<toolchain>` resolves (i.e. rustup has it installed).
fn toolchain_available(toolchain: &str) -> bool {
    Command::new("cargo")
        .arg(format!("+{toolchain}"))
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── Fixture ─────────────────────────────────────────────────────────────────

/// A throwaway crate + frontend project in a temp git repo. Cleaned up on drop
/// unless `TF_TRACK_KEEP` is set (handy for debugging a failure).
struct Fixture {
    dir: PathBuf,
    pkg: String,
    toolchain: String,
}

impl Fixture {
    fn scaffold(tag: &str, with_build_rs: bool, toolchain: &str) -> Self {
        let dir = env::temp_dir().join(format!("tf-track-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let client = dir.join("client");
        fs::create_dir_all(client.join("src")).unwrap();
        // An empty node_modules makes the macro's `install_if_needed` a no-op, so
        // the fixture never shells out to a package manager.
        fs::create_dir_all(client.join("node_modules")).unwrap();
        // package.json presence selects BUILD mode (compile-time build + embed).
        fs::write(client.join("package.json"), "{}\n").unwrap();

        let pkg = format!("tf_fixture_{tag}");
        fs::write(dir.join("Cargo.toml"), cargo_toml(&pkg, with_build_rs)).unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), MAIN_RS).unwrap();
        if with_build_rs {
            fs::write(dir.join("build.rs"), BUILD_RS).unwrap();
        }

        // A .gitignore for realism (mirrors a real project's build output). It is
        // deliberately *not* load-bearing: the tracking fix pins the build script to
        // explicit source paths, so the properties hold with or without VCS. (An earlier
        // design leaned on cargo's ignore-aware whole-package watching to exclude dist/;
        // that turned out not to hold even in a committed repo, which is what these tests
        // guard against — so no `git init` is needed here.)
        fs::write(
            dir.join(".gitignore"),
            "/dist\n/client/dist\n/client/node_modules\n",
        )
        .unwrap();

        Fixture {
            dir,
            pkg,
            toolchain: toolchain.to_string(),
        }
    }

    fn client(&self) -> PathBuf {
        self.dir.join("client")
    }

    /// Overwrite the single tracked source file with a unique token.
    fn set_content(&self, token: &str) {
        fs::write(self.client().join("src/content.txt"), format!("{token}\n")).unwrap();
    }

    /// Each fixture gets its own target dir inside its (fresh) temp dir. A shared
    /// target dir would carry stale fingerprints between runs — fixture paths are
    /// PID-keyed but a shared target name is not — which masks or fabricates the
    /// very churn these tests measure. Isolation costs a dependency recompile per
    /// scenario; correctness is worth it for an `#[ignore]`d test.
    fn target_dir(&self) -> PathBuf {
        self.dir.join("target")
    }

    fn build(&self) -> Output {
        self.cargo_build(&[])
    }

    /// `cargo build --all-targets`, which compiles the bin *and* its test target —
    /// two compilation units cargo runs concurrently, each expanding the same
    /// `frontend!` in its own proc-macro process.
    fn build_all_targets(&self) -> Output {
        self.cargo_build(&["--all-targets"])
    }

    fn cargo_build(&self, extra: &[&str]) -> Output {
        Command::new("cargo")
            .arg(format!("+{}", self.toolchain))
            .arg("build")
            .args(extra)
            .current_dir(&self.dir)
            .env("CARGO_TARGET_DIR", self.target_dir())
            .output()
            .expect("failed to spawn cargo")
    }

    /// How many times the fixture's frontend build command has run.
    ///
    /// The counter lives under `node_modules`, which the macro excludes from source
    /// tracking — so incrementing it cannot itself invalidate the build and skew the
    /// number it reports.
    fn build_count(&self) -> usize {
        fs::read_to_string(self.client().join("node_modules/build_count"))
            .map(|counter| counter.lines().count())
            .unwrap_or(0)
    }

    fn binary(&self) -> PathBuf {
        let name = if cfg!(windows) {
            format!("{}.exe", self.pkg)
        } else {
            self.pkg.clone()
        };
        self.target_dir().join("debug").join(name)
    }

    fn binary_contains(&self, needle: &[u8]) -> bool {
        let bytes = fs::read(self.binary()).expect("read fixture binary");
        bytes.windows(needle.len()).any(|w| w == needle)
    }

    fn dist_index_mtime(&self) -> SystemTime {
        fs::metadata(self.client().join("dist/index.html"))
            .expect("dist/index.html should exist after a build")
            .modified()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if env::var_os("TF_TRACK_KEEP").is_none() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn cargo_toml(pkg: &str, with_build_rs: bool) -> String {
    let root = Path::new(CRATE_ROOT).display();
    let build_deps = if with_build_rs {
        format!("\n[build-dependencies]\ntrillium-frontend = {{ path = \"{root}\" }}\n")
    } else {
        String::new()
    };
    format!(
        "[package]\n\
         name = \"{pkg}\"\n\
         version = \"0.0.0\"\n\
         edition = \"2024\"\n\
         publish = false\n\
         \n\
         # Own workspace so cargo doesn't walk up the temp dir looking for one.\n\
         [workspace]\n\
         \n\
         [dependencies]\n\
         trillium-frontend = {{ path = \"{root}\" }}\n\
         {build_deps}"
    )
}

/// Constructs the handler (which forces the compile-time build + embed) and
/// `black_box`es it so the embedded assets can't be optimized out of the binary.
///
/// The build appends to a counter under `node_modules` (untracked, so counting cannot
/// perturb what it measures) — that's how [`Fixture::build_count`] observes how many
/// times the frontend actually built.
const MAIN_RS: &str = r#"use trillium_frontend::frontend;

fn main() {
    std::hint::black_box(frontend!(
        path = "./client",
        build = "mkdir -p dist && cp src/content.txt dist/index.html && echo ran >> node_modules/build_count",
        dist = "dist"
    ));
}
"#;

const BUILD_RS: &str = "fn main() {\n    trillium_frontend::build::track_frontend_sources();\n}\n";

// ── Shared scenario ─────────────────────────────────────────────────────────

fn assert_build_ok(out: &Output, context: &str) {
    assert!(
        out.status.success(),
        "cargo build failed ({context}):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The whole property, exercised once per (toolchain, build.rs) combination so
/// the expensive dependency compile is amortized across all assertions.
fn run_scenario(toolchain: &str, with_build_rs: bool) {
    if !toolchain_available(toolchain) {
        eprintln!("SKIP: `{toolchain}` toolchain not installed");
        return;
    }

    let tag = format!(
        "{toolchain}_{}",
        if with_build_rs { "brs" } else { "nobrs" }
    );
    let fx = Fixture::scaffold(&tag, with_build_rs, toolchain);

    // Build #1 with token ALPHA embedded.
    let alpha = format!("TF_TOKEN_{tag}_ALPHA");
    fx.set_content(&alpha);
    assert_build_ok(&fx.build(), "initial build");

    // Build #2: on stable, the precise `rerun-if-changed` watch only takes over
    // after the build script has re-run once with the manifest present. Harmless
    // no-op on nightly. After this the fixture is in steady state.
    assert_build_ok(&fx.build(), "arming rebuild");
    assert!(
        fx.binary_contains(alpha.as_bytes()),
        "initial build did not embed the expected assets ({tag})"
    );

    // Property 2 — no over-build: a no-op `cargo build` must not re-run the
    // frontend build (which would bump dist/index.html's mtime).
    let before = fx.dist_index_mtime();
    assert_build_ok(&fx.build(), "no-op rebuild");
    assert_eq!(
        before,
        fx.dist_index_mtime(),
        "frontend build re-ran on a no-op `cargo build` — over-build ({tag})"
    );

    // Property 1 — anti-stale: editing *only* a frontend source and rebuilding
    // must embed the new assets.
    let bravo = format!("TF_TOKEN_{tag}_BRAVO");
    fx.set_content(&bravo);
    assert_build_ok(&fx.build(), "rebuild after source edit");
    assert!(
        fx.binary_contains(bravo.as_bytes()),
        "STALE: a frontend source edit was not reflected in the embedded assets ({tag})"
    );
}

// ── Scenarios ───────────────────────────────────────────────────────────────

/// Nightly, zero setup: the macro tracks sources itself via `proc_macro_tracked_path`.
#[test]
#[ignore = "runs a real cargo build; needs the nightly toolchain"]
fn nightly_native_tracking() {
    run_scenario("nightly", /* with_build_rs = */ false);
}

/// Stable: the macro can't track files, so the `build.rs` shim must carry the property.
#[test]
#[ignore = "runs a real cargo build; needs the stable toolchain"]
fn stable_shim_tracking() {
    run_scenario("stable", /* with_build_rs = */ true);
}

/// Nightly with the shim also present ("belt and suspenders"): native tracking is
/// active, the shim stands down (sentinel), and — crucially — this must not devolve
/// into a rebuild-every-time loop from cargo's whole-package watch seeing dist churn.
#[test]
#[ignore = "runs a real cargo build; needs the nightly toolchain"]
fn nightly_with_shim_present() {
    run_scenario("nightly", /* with_build_rs = */ true);
}

/// Property 3 — build once: several compilation units expanding one `frontend!`
/// concurrently must produce exactly one frontend build.
///
/// `cargo build --all-targets` compiles the bin and its test target in parallel, each
/// expanding the macro in a separate process. Unserialized, both build: they race to
/// write one `dist` while the other's `static_compiled!` reads it, which fails the
/// build outright with a bare `No such file or directory (os error 2)`.
///
/// Counting builds rather than trying to catch that crash is deliberate. The crash
/// needs unlucky timing and reproduces only sometimes; "how many times did the build
/// run" is the underlying property, and it is exactly deterministic.
#[test]
#[ignore = "runs a real cargo build; needs the nightly toolchain"]
fn concurrent_expansion_builds_once() {
    if !toolchain_available("nightly") {
        eprintln!("SKIP: `nightly` toolchain not installed");
        return;
    }

    let fx = Fixture::scaffold("concurrent", /* with_build_rs = */ false, "nightly");
    fx.set_content("TF_TOKEN_concurrent");
    assert_build_ok(&fx.build_all_targets(), "concurrent --all-targets build");

    assert_eq!(
        fx.build_count(),
        1,
        "one `frontend!` expanded concurrently ran the frontend build {} times; \
         concurrent expansions must be serialized and redundant builds skipped, or \
         they clobber each other's dist",
        fx.build_count()
    );
}
