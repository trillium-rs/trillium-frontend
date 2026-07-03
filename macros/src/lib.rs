// Enabled automatically on nightly by build.rs's probe (see `has_tracked_path`).
#![cfg_attr(has_tracked_path, feature(proc_macro_tracked_path))]
#![forbid(unsafe_code)]
use proc_macro::{TokenStream, TokenTree};
use quote::quote;
use std::path::{Path, PathBuf};

/// Internal proc macro invoked by the `frontend!` macro_rules wrapper.
/// Do not call directly; use `trillium_frontend::frontend!` instead.
#[proc_macro]
pub fn frontend_impl(input: TokenStream) -> TokenStream {
    let is_dev_proxy = cfg!(feature = "dev-proxy");
    let args = parse_args(input);

    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set by cargo");
    let project_path = resolve_path(&args.path, &manifest_dir);
    let project_path_str = project_path
        .to_str()
        .expect("project path is not valid UTF-8");

    let source_exists = project_path.join("package.json").exists();

    if !source_exists {
        // Prebuilt mode: embed existing dist assets without building
        return emit_prebuilt(&project_path, project_path_str, &args).into();
    }

    let detection = detect(&project_path);
    install_if_needed(&project_path, &detection);

    if is_dev_proxy {
        // Dev-proxy mode: spawn dev server, proxy to it
        let dev_command_tokens = match detection.full_dev_command() {
            Some(cmd) => quote!(Some(#cmd)),
            None => quote!(None),
        };
        quote! {
            FrontendHandler::new(
                None,
                None,
                #project_path_str,
                #dev_command_tokens,
            )
        }
    } else {
        // Build mode: run build command at compile time, embed output
        let detected_build = detection.full_build_command();
        let build_command = args
            .build
            .as_deref()
            .or(detected_build.as_deref())
            .expect("trillium-frontend: could not detect a build command; specify build = \"...\" in the macro")
            .to_string();

        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(&build_command)
            .current_dir(&project_path)
            .status()
            .unwrap_or_else(|e| panic!("trillium-frontend: failed to run build `{build_command}`: {e}"));

        if !status.success() {
            panic!("trillium-frontend: build command `{build_command}` failed with {status}");
        }

        let dist_dir = project_path.join(
            args.dist
                .as_deref()
                .or(detection.dist.as_deref())
                .unwrap_or("dist"),
        );

        // Register the source tree as a compile dependency so edits trigger a rebuild.
        track_sources(&project_path, &dist_dir);

        let dist_str = dist_dir
            .to_str()
            .expect("dist path is not valid UTF-8");

        let index_html = dist_dir.join("index.html");
        let spa_fallback_tokens = if index_html.exists() {
            let index_str = index_html
                .to_str()
                .expect("index.html path is not valid UTF-8");
            quote! {
                Some(static_compiled!(#index_str))
            }
        } else {
            quote!(None)
        };

        quote! {
            FrontendHandler::new(
                Some(static_compiled!(#dist_str)),
                #spa_fallback_tokens,
                #project_path_str,
                None,
            )
        }
    }
    .into()
}

fn emit_prebuilt(
    project_path: &Path,
    project_path_str: &str,
    args: &MacroArgs,
) -> proc_macro2::TokenStream {
    let dist_dir = project_path.join(args.dist.as_deref().unwrap_or("dist"));

    if !dist_dir.exists() {
        panic!(
            "trillium-frontend: no source (package.json) and no pre-built dist directory found at `{}`. \
             Either include the dist directory in your published crate or ensure the frontend source is available.",
            dist_dir.display()
        );
    }

    let dist_str = dist_dir.to_str().expect("dist path is not valid UTF-8");

    let index_html = dist_dir.join("index.html");
    let spa_fallback_tokens = if index_html.exists() {
        let index_str = index_html
            .to_str()
            .expect("index.html path is not valid UTF-8");
        quote! {
            Some(static_compiled!(#index_str))
        }
    } else {
        quote!(None)
    };

    quote! {
        FrontendHandler::new(
            Some(static_compiled!(#dist_str)),
            #spa_fallback_tokens,
            #project_path_str,
            None,
        )
    }
}

// ── Source tracking ───────────────────────────────────────────────────────────

/// Register the frontend source tree as a compile-time dependency so cargo re-runs the
/// build when a source file is added, removed, or modified.
///
/// - On nightly, calls [`proc_macro::tracked::path`] directly (no downstream build.rs, no
///   sources embedded in the binary). Enabled by build.rs's channel + API probe.
/// - Always, if `OUT_DIR` is set (i.e. the downstream crate has a build script), writes a
///   manifest of tracked paths into `OUT_DIR` for the build-script shim
///   (`trillium_frontend::build::track_frontend_sources`) to emit as `rerun-if-changed`.
///   The proc macro and that build script share the same `OUT_DIR`. The shim emits those
///   paths on every toolchain — even nightly, where native tracking is also active — so the
///   build script never falls back to cargo's package-mtime change detection (which can
///   loop; see the `build` module docs).
fn track_sources(project_path: &Path, dist_dir: &Path) {
    let mut tracked = Vec::new();
    collect_paths(project_path, dist_dir, &mut tracked);

    register_tracked(&tracked);

    if let Ok(out_dir) = std::env::var("OUT_DIR") {
        write_manifest(&out_dir, project_path, &tracked);
    }
}

#[cfg(has_tracked_path)]
fn register_tracked(paths: &[PathBuf]) {
    for path in paths {
        proc_macro::tracked::path(path);
    }
}

#[cfg(not(has_tracked_path))]
fn register_tracked(_paths: &[PathBuf]) {}

/// Recursively collect files and directories under `dir`, skipping dependency/output
/// directories. Directories are included too, so that adding or removing an entry (which
/// bumps the directory's mtime) also triggers a rebuild — modifications are caught by the
/// per-file entries. Symlinks are not followed, to avoid cycles.
fn collect_paths(dir: &Path, dist_dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Track the directory itself so added/removed entries trigger a rebuild — but NOT if it
    // contains the build output (the dist dir or an ancestor of it, including the project
    // root). The build rewrites dist on every run, bumping those directories' mtimes, which
    // would otherwise cause a perpetual rebuild loop.
    if !dist_dir.starts_with(dir) {
        out.push(dir.to_path_buf());
    }
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            let name = entry.file_name();
            let excluded = matches!(name.to_str(), Some("node_modules" | ".git" | "target"))
                || path == dist_dir;
            if !excluded {
                collect_paths(&path, dist_dir, out);
            }
        } else if file_type.is_file() {
            out.push(path);
        }
    }
}

/// Write the tracked-path manifest to `OUT_DIR`, keyed by a hash of the project path so
/// multiple `frontend!` invocations in one crate don't collide.
fn write_manifest(out_dir: &str, project_path: &Path, tracked: &[PathBuf]) {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    project_path.hash(&mut hasher);
    let file_name = format!("trillium-frontend-{:016x}.paths", hasher.finish());

    let mut body = String::new();
    for path in tracked {
        if let Some(path) = path.to_str() {
            body.push_str(path);
            body.push('\n');
        }
    }
    let _ = std::fs::write(Path::new(out_dir).join(file_name), body);
}

// ── Argument parsing ──────────────────────────────────────────────────────────

#[derive(Default)]
struct MacroArgs {
    path: String,
    build: Option<String>,
    dist: Option<String>,
}

fn parse_args(input: TokenStream) -> MacroArgs {
    let tokens: Vec<_> = input.into_iter().collect();
    match tokens.as_slice() {
        // frontend!("./client")
        [TokenTree::Literal(lit)] => MacroArgs {
            path: unwrap_string_literal(lit),
            ..Default::default()
        },
        // frontend!(path = "...", build = "...", dist = "...")
        _ => parse_key_value_args(&tokens),
    }
}

fn parse_key_value_args(tokens: &[TokenTree]) -> MacroArgs {
    let mut args = MacroArgs::default();
    let mut i = 0;
    while i < tokens.len() {
        let key = match &tokens[i] {
            TokenTree::Ident(id) => id.to_string(),
            TokenTree::Punct(p) if p.as_char() == ',' => {
                i += 1;
                continue;
            }
            other => panic!("trillium-frontend: expected key identifier, got {other}"),
        };
        i += 1;
        // `=`
        match &tokens[i] {
            TokenTree::Punct(p) if p.as_char() == '=' => i += 1,
            other => panic!("trillium-frontend: expected `=` after `{key}`, got {other}"),
        }
        let value = match &tokens[i] {
            TokenTree::Literal(lit) => unwrap_string_literal(lit),
            other => panic!("trillium-frontend: expected string literal for `{key}`, got {other}"),
        };
        i += 1;
        match key.as_str() {
            "path" => args.path = value,
            "build" => args.build = Some(value),
            "dist" => args.dist = Some(value),
            other => {
                panic!("trillium-frontend: unknown key `{other}`; valid keys: path, build, dist")
            }
        }
    }
    if args.path.is_empty() {
        panic!("trillium-frontend: `path` is required");
    }
    args
}

fn unwrap_string_literal(lit: &proc_macro::Literal) -> String {
    let repr = lit.to_string();
    if repr.starts_with('"') && repr.ends_with('"') {
        repr[1..repr.len() - 1].to_string()
    } else {
        panic!("trillium-frontend: expected a string literal, got {repr}")
    }
}

fn resolve_path(raw: &str, manifest_dir: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    let base = if p.is_relative() {
        PathBuf::from(manifest_dir).join(p)
    } else {
        p
    };
    base.canonicalize()
        .unwrap_or_else(|e| panic!("trillium-frontend: could not resolve path `{raw}`: {e}"))
}

// ── Detection ─────────────────────────────────────────────────────────────────

struct Detection {
    pkg_manager: Option<PkgManager>,
    framework: Option<Framework>,
    /// dist dir name (relative to project root)
    pub dist: Option<String>,
}

impl Detection {
    /// `bun run vite`, `npx vite`, etc.
    fn full_dev_command(&self) -> Option<String> {
        let fw = self.framework.as_ref()?;
        let prefix = self
            .pkg_manager
            .as_ref()
            .map(PkgManager::run_prefix)
            .unwrap_or("npx");
        Some(format!("{prefix} {}", fw.dev_command()))
    }

    fn full_build_command(&self) -> Option<String> {
        let fw = self.framework.as_ref()?;
        let prefix = self
            .pkg_manager
            .as_ref()
            .map(PkgManager::run_prefix)
            .unwrap_or("npx");
        Some(format!("{prefix} {}", fw.build_command()))
    }
}

#[derive(Clone, Copy)]
enum PkgManager {
    Bun,
    Pnpm,
    Yarn,
    Npm,
}

impl PkgManager {
    fn run_prefix(&self) -> &'static str {
        match self {
            PkgManager::Bun => "bun run",
            PkgManager::Pnpm => "pnpm exec",
            PkgManager::Yarn => "yarn exec",
            PkgManager::Npm => "npx",
        }
    }

    fn install_command(&self) -> &'static str {
        match self {
            PkgManager::Bun => "bun install",
            PkgManager::Pnpm => "pnpm install",
            PkgManager::Yarn => "yarn install",
            PkgManager::Npm => "npm install",
        }
    }
}

#[derive(Clone, Copy)]
enum Framework {
    Vite,
    Webpack,
    Next,
}

impl Framework {
    fn dev_command(self) -> &'static str {
        match self {
            Framework::Vite => "vite --strictPort --clearScreen false",
            Framework::Webpack => "webpack serve",
            Framework::Next => "next dev",
        }
    }

    fn build_command(self) -> &'static str {
        match self {
            Framework::Vite => "vite build",
            Framework::Webpack => "webpack build",
            Framework::Next => "next build",
        }
    }

    fn dist_dir(self) -> &'static str {
        match self {
            Framework::Vite | Framework::Webpack => "dist",
            Framework::Next => ".next",
        }
    }
}

fn install_if_needed(project_path: &Path, detection: &Detection) {
    if project_path.join("node_modules").exists() {
        return;
    }

    let install_command = detection
        .pkg_manager
        .as_ref()
        .map(PkgManager::install_command)
        .unwrap_or("npm install");

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(install_command)
        .current_dir(project_path)
        .status()
        .unwrap_or_else(|e| panic!("trillium-frontend: failed to run `{install_command}`: {e}"));

    if !status.success() {
        panic!("trillium-frontend: `{install_command}` failed with {status}");
    }
}

fn detect(project_path: &Path) -> Detection {
    let pkg_manager = detect_pkg_manager(project_path);
    let framework = detect_framework(project_path);

    Detection {
        dist: framework.map(|f| f.dist_dir().to_string()),
        pkg_manager,
        framework,
    }
}

fn detect_pkg_manager(path: &Path) -> Option<PkgManager> {
    let candidates = [
        ("bun.lockb", PkgManager::Bun),
        ("bun.lock", PkgManager::Bun),
        ("pnpm-lock.yaml", PkgManager::Pnpm),
        ("yarn.lock", PkgManager::Yarn),
        ("package-lock.json", PkgManager::Npm),
    ];
    candidates
        .iter()
        .find(|(f, _)| path.join(f).exists())
        .map(|(_, pm)| *pm)
}

fn detect_framework(path: &Path) -> Option<Framework> {
    let vite_configs = ["vite.config.js", "vite.config.ts", "vite.config.mjs"];
    let webpack_configs = [
        "webpack.config.js",
        "webpack.config.ts",
        "webpack.config.mjs",
    ];
    let next_configs = ["next.config.js", "next.config.ts", "next.config.mjs"];

    if vite_configs.iter().any(|f| path.join(f).exists()) {
        return Some(Framework::Vite);
    }
    if webpack_configs.iter().any(|f| path.join(f).exists()) {
        return Some(Framework::Webpack);
    }
    if next_configs.iter().any(|f| path.join(f).exists()) {
        return Some(Framework::Next);
    }
    None
}
