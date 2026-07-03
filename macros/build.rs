//! Build script for `trillium-frontend-macros`.
//!
//! Its only job is to probe whether the current compiler supports the unstable
//! `proc_macro_tracked_path` feature *in the exact shape we call it*, and, if so,
//! set the `has_tracked_path` cfg. The proc macro then uses `proc_macro::tracked::path`
//! to tell the compiler which frontend source files it depends on — without a build
//! script in the downstream crate, and without embedding the sources in the binary.
//!
//! We probe the real call site (not just the feature gate) on purpose: this unstable
//! API has already been renamed/reshaped once (`tracked_path::path` -> `tracked::path`,
//! and the signature became generic over `AsRef<Path>`). Probing the actual call means
//! a future rename makes us silently fall back to the build-script shim instead of
//! breaking the build.
//!
//! There is no cargo feature to opt in: this is enabled automatically on nightly. We gate
//! the probe on the *nightly channel* (parsed from `rustc -vV`) rather than merely on the
//! feature compiling, so that a stable toolchain running under `RUSTC_BOOTSTRAP=1` — where
//! unstable features happen to compile — does *not* silently activate this. Real nightly is
//! required. The probe then guards against the API churning out from under us.

use std::{env, fs, process::Command};

fn main() {
    // Always declare the cfg so `cfg!(has_tracked_path)` / `#[cfg(has_tracked_path)]`
    // don't trip the `unexpected_cfgs` lint, whether or not we end up setting it.
    println!("cargo::rustc-check-cfg=cfg(has_tracked_path)");

    if is_nightly() && probe() {
        println!("cargo::rustc-cfg=has_tracked_path");
    }
}

/// True only on a genuine nightly toolchain. Parses the `release:` line of `rustc -vV`,
/// which ends in `-nightly` for nightly (vs `-beta`, `-dev`, or a bare version on stable).
/// Deliberately does *not* trust `RUSTC_BOOTSTRAP`-enabled stable compilers.
fn is_nightly() -> bool {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let Ok(output) = Command::new(rustc).arg("-vV").output() else {
        return false;
    };
    let Ok(version) = String::from_utf8(output.stdout) else {
        return false;
    };
    version
        .lines()
        .find_map(|line| line.strip_prefix("release:"))
        .is_some_and(|release| release.trim().ends_with("-nightly"))
}

/// Compile a tiny snippet that calls `proc_macro::tracked::path` exactly the way the
/// macro does. Returns true only if it compiles clean on this toolchain.
fn probe() -> bool {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let Ok(out_dir) = env::var("OUT_DIR") else {
        return false;
    };
    let probe_src = format!("{out_dir}/probe_tracked_path.rs");
    let snippet = "#![feature(proc_macro_tracked_path)]\n\
        extern crate proc_macro;\n\
        #[allow(dead_code)]\n\
        fn _probe<P: AsRef<std::path::Path>>(p: P) { proc_macro::tracked::path(p); }\n";
    if fs::write(&probe_src, snippet).is_err() {
        return false;
    }

    Command::new(rustc)
        .args([
            // Match this crate's edition so the probe reflects the real build conditions.
            "--edition=2024",
            "--crate-type=proc-macro",
            "--emit=metadata",
            "--out-dir",
            &out_dir,
            &probe_src,
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
