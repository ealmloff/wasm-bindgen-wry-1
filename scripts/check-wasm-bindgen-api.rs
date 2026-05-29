#!/bin/sh
//usr/bin/env rustc --edition=2024 "$0" -o "${TMPDIR:-/tmp}/check-wasm-bindgen-api" && CHECK_WASM_BINDGEN_API_SOURCE="$0" "${TMPDIR:-/tmp}/check-wasm-bindgen-api" "$@"; exit $?

// Run directly with:
//   ./scripts/check-wasm-bindgen-api.rs [wasm-bindgen-ref]
//
// Or compile manually with:
//   rustc --edition=2024 scripts/check-wasm-bindgen-api.rs -o /tmp/check-wasm-bindgen-api
//   /tmp/check-wasm-bindgen-api [wasm-bindgen-ref]

use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PACKAGE: &str = "wasm-bindgen";
const CURRENT_MANIFEST: &str = "shims/wasm-bindgen/Cargo.toml";

#[derive(Debug)]
struct Error(String);

impl Error {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

type Result<T> = std::result::Result<T, Error>;

#[derive(Default)]
struct Args {
    upstream_ref: Option<String>,
    target: Option<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let upstream_ref = match args.upstream_ref {
        Some(upstream_ref) => upstream_ref,
        None => default_upstream_ref()?,
    };
    let repo_root = repo_root()?;
    let wasm_bindgen_submodule = repo_root.join("wasm-bindgen");
    if !wasm_bindgen_submodule.is_dir() {
        return Err(Error::new(format!(
            "missing wasm-bindgen submodule at {}",
            wasm_bindgen_submodule.display()
        )));
    }

    verify_git_ref(&wasm_bindgen_submodule, &upstream_ref)?;

    let tmp = TempDir::new("check-wasm-bindgen-api")?;
    let baseline_root = tmp.path().join("upstream-wasm-bindgen");
    fs::create_dir_all(&baseline_root)?;
    extract_git_archive(&wasm_bindgen_submodule, &upstream_ref, &baseline_root)?;
    hide_upstream_convert_module(&baseline_root)?;

    println!("Checking wasm-bindgen API compatibility with cargo-semver-checks");
    println!("baseline upstream ref: {upstream_ref}");
    println!("baseline root: {}", baseline_root.display());
    println!("current manifest: {CURRENT_MANIFEST}");
    println!("baseline adjustment: #[doc(hidden)] on upstream wasm_bindgen::convert APIs");
    if let Some(target) = args.target.as_deref() {
        println!("target: {target}");
    }
    println!();

    run_semver_checks(&repo_root, &baseline_root, args.target.as_deref())
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let mut raw = env::args().skip(1);

    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage();
                process::exit(0);
            }
            "--target" => {
                args.target = Some(
                    raw.next()
                        .ok_or_else(|| Error::new("--target requires a value"))?,
                );
            }
            _ => {
                if let Some(target) = arg.strip_prefix("--target=") {
                    args.target = Some(target.to_string());
                } else if args.upstream_ref.is_none() {
                    args.upstream_ref = Some(arg);
                } else {
                    return Err(Error::new(format!("unexpected argument `{arg}`")));
                }
            }
        }
    }

    Ok(args)
}

fn print_usage() {
    println!(
        "\
Usage:
  check-wasm-bindgen-api [wasm-bindgen-ref] [--target TARGET]

Compares the local wasm-bindgen shim against a clean upstream wasm-bindgen tree
with `cargo semver-checks --baseline-root`.

If wasm-bindgen-ref is omitted, patches/wasm-bindgen/BASE is used when present,
otherwise HEAD is used."
    );
}

fn default_upstream_ref() -> Result<String> {
    let repo_root = repo_root()?;
    let patch_base = repo_root.join("patches/wasm-bindgen/BASE");
    if patch_base.is_file() {
        let text = fs::read_to_string(patch_base)?;
        if let Some(line) = text
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            return Ok(line.to_string());
        }
    }
    Ok("HEAD".to_string())
}

fn repo_root() -> Result<PathBuf> {
    if let Some(root) = repo_root_from_source_env()? {
        return Ok(root);
    }

    let current_dir = env::current_dir()?;
    for ancestor in current_dir.ancestors() {
        if ancestor.join("Cargo.toml").is_file()
            && ancestor.join("wry-bindgen").is_dir()
            && ancestor.join("shims/wasm-bindgen").is_dir()
        {
            return Ok(ancestor.to_path_buf());
        }
    }

    Err(Error::new(
        "could not find repo root; run from inside wasm-bindgen-wry",
    ))
}

fn repo_root_from_source_env() -> Result<Option<PathBuf>> {
    let Ok(source_path) = env::var("CHECK_WASM_BINDGEN_API_SOURCE") else {
        return Ok(None);
    };
    let source_path = PathBuf::from(source_path);
    let source_path = if source_path.is_absolute() {
        source_path
    } else {
        env::current_dir()?.join(source_path)
    };

    let Some(parent) = source_path.parent() else {
        return Ok(None);
    };
    if parent.file_name().is_some_and(|name| name == "scripts") {
        return Ok(parent.parent().map(Path::to_path_buf));
    }
    Ok(None)
}

fn verify_git_ref(repo: &Path, git_ref: &str) -> Result<()> {
    let tree_ref = format!("{git_ref}^{{tree}}");
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(&tree_ref)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| Error::new(format!("failed to run git rev-parse: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "could not resolve wasm-bindgen ref `{git_ref}`"
        )))
    }
}

fn extract_git_archive(repo: &Path, git_ref: &str, output_dir: &Path) -> Result<()> {
    let archive_path = output_dir.with_extension("tar");
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("archive")
        .arg("--format=tar")
        .arg(format!("--output={}", archive_path.display()))
        .arg(git_ref)
        .status()
        .map_err(|error| Error::new(format!("failed to run git archive: {error}")))?;
    if !status.success() {
        return Err(Error::new(format!(
            "git archive failed for wasm-bindgen ref `{git_ref}`"
        )));
    }

    let status = Command::new("tar")
        .arg("-xf")
        .arg(&archive_path)
        .arg("-C")
        .arg(output_dir)
        .status()
        .map_err(|error| Error::new(format!("failed to run tar: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(format!(
            "failed to extract {}",
            archive_path.display()
        )))
    }
}

fn hide_upstream_convert_module(baseline_root: &Path) -> Result<()> {
    let lib_rs = baseline_root.join("src/lib.rs");
    let text = fs::read_to_string(&lib_rs)?;
    let updated = hide_convert_module_text(&text).ok_or_else(|| {
        Error::new(format!(
            "could not find `pub mod convert;` in {}",
            lib_rs.display()
        ))
    })?;

    if updated != text {
        fs::write(&lib_rs, updated)?;
    }

    Ok(())
}

fn hide_convert_module_text(text: &str) -> Option<String> {
    let replacements = [
        ("pub mod convert;", "#[doc(hidden)]\npub mod convert;"),
        (
            "    pub use crate::convert::Upcast; // provides upcast() and upcast_ref()",
            "    #[doc(hidden)]\n    pub use crate::convert::Upcast; // provides upcast() and upcast_ref()",
        ),
        (
            "pub use crate::convert::{IntoJsGeneric, JsGeneric};",
            "#[doc(hidden)]\npub use crate::convert::{IntoJsGeneric, JsGeneric};",
        ),
    ];

    let mut updated = text.to_string();
    for (plain, hidden) in replacements {
        if updated.contains(hidden) {
            continue;
        }

        if !updated.contains(plain) {
            return None;
        }

        updated = updated.replacen(plain, hidden, 1);
    }

    Some(updated)
}

fn run_semver_checks(repo_root: &Path, baseline_root: &Path, target: Option<&str>) -> Result<()> {
    let manifest = repo_root.join(CURRENT_MANIFEST);
    let mut command = Command::new("cargo");
    command
        .arg("semver-checks")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--package")
        .arg(PACKAGE)
        .arg("--baseline-root")
        .arg(baseline_root)
        .arg("--all-features")
        .arg("--release-type")
        .arg("patch")
        .arg("--color")
        .arg("never");

    if let Some(target) = target {
        command.arg("--target").arg(target);
    }

    let status = command
        .status()
        .map_err(|error| Error::new(format!("failed to run cargo semver-checks: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::new(match status.code() {
            Some(code) => format!("cargo semver-checks failed with status {code}"),
            None => "cargo semver-checks terminated by signal".to_string(),
        }))
    }
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self> {
        let mut path = env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::new(format!("system time is before unix epoch: {error}")))?
            .as_nanos();
        path.push(format!("{prefix}-{}-{nanos}", process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::hide_convert_module_text;

    #[test]
    fn hides_convert_module_declaration() {
        let input = "\
pub mod prelude {
    pub use crate::convert::Upcast; // provides upcast() and upcast_ref()
}
pub mod closure;
pub mod convert;
pub mod describe;
pub use crate::convert::{IntoJsGeneric, JsGeneric};
";
        let output = hide_convert_module_text(input).expect("convert module should be found");
        assert_eq!(
            output,
            "\
pub mod prelude {
    #[doc(hidden)]
    pub use crate::convert::Upcast; // provides upcast() and upcast_ref()
}
pub mod closure;
#[doc(hidden)]
pub mod convert;
pub mod describe;
#[doc(hidden)]
pub use crate::convert::{IntoJsGeneric, JsGeneric};
"
        );
    }

    #[test]
    fn hide_convert_module_is_idempotent() {
        let input = "\
pub mod prelude {
    #[doc(hidden)]
    pub use crate::convert::Upcast; // provides upcast() and upcast_ref()
}
#[doc(hidden)]
pub mod convert;
#[doc(hidden)]
pub use crate::convert::{IntoJsGeneric, JsGeneric};
";
        let output = hide_convert_module_text(input).expect("convert module should be found");
        assert_eq!(output, input);
    }

    #[test]
    fn missing_convert_module_returns_none() {
        assert!(hide_convert_module_text("pub mod closure;\n").is_none());
    }
}
