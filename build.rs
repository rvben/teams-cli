//! Sets `TEAMS_BUILD` to the commit this binary was built from, so the WSL helper
//! handshake can tell two builds of the same version apart. A build with uncommitted
//! changes gets `-dirty-` and a digest of those changes. Outside a git checkout the
//! commit comes from the `.cargo_vcs_info.json` that `cargo package` writes; without
//! either it stays empty and only the version is compared. `TEAMS_LONG_VERSION` adds
//! it to `--version`.
use std::{path::Path, process::Command};

fn main() {
    for path in ["build.rs", "Cargo.toml", "Cargo.lock", "src"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let build = from_git().or_else(from_package).unwrap_or_default();
    println!("cargo:rustc-env=TEAMS_BUILD={build}");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let long = match build.as_str() {
        "" => version,
        build => format!("{version} ({build})"),
    };
    println!("cargo:rustc-env=TEAMS_LONG_VERSION={long}");
}

fn git(args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git").args(args).output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn text(bytes: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(bytes).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn from_git() -> Option<String> {
    // Only the repository this crate is the root of; never a parent checkout.
    let top = text(git(&["rev-parse", "--show-toplevel"])?)?;
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    if Path::new(&top).canonicalize().ok()? != Path::new(&manifest).canonicalize().ok()? {
        return None;
    }
    // A missing path would rerun this script on every build, so only existing ones.
    let reference = git(&["symbolic-ref", "-q", "HEAD"]).and_then(text);
    for name in ["HEAD", "index", "packed-refs"]
        .into_iter()
        .chain(reference.as_deref())
    {
        if let Some(path) = git(&["rev-parse", "--git-path", name]).and_then(text)
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let commit = text(git(&["rev-parse", "--short=12", "HEAD"])?)?;
    let changes = git(&["diff", "HEAD", "--no-ext-diff", "--binary"])?;
    Some(if changes.is_empty() {
        commit
    } else {
        format!("{commit}-dirty-{:08x}", fnv1a(&changes) as u32)
    })
}

fn from_package() -> Option<String> {
    let info = std::fs::read_to_string(".cargo_vcs_info.json").ok()?;
    let sha1 = info.split("\"sha1\"").nth(1)?.split('"').nth(1)?;
    let commit = sha1
        .get(..12)
        .filter(|c| c.bytes().all(|b| b.is_ascii_hexdigit()))?;
    let dirty = info.contains("\"dirty\": true") || info.contains("\"dirty\":true");
    Some(if dirty {
        format!("{commit}-dirty")
    } else {
        commit.to_owned()
    })
}

/// FNV-1a, so the digest is the same whichever toolchain builds the other side.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
