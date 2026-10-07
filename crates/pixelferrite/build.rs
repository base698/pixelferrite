use std::{env, path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    // Always refresh provenance, including after an empty commit, a branch switch,
    // or a change in pf-core. This intentionally absent path makes Cargo recheck
    // Git on each invocation; dependency crates still use their normal cache.
    println!("cargo::rerun-if-changed={}/always-recheck-build-info", env::var("OUT_DIR").unwrap());
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let root = Path::new(&manifest).join("../..");
    // An unpacked source archive inside another checkout must not borrow its SHA.
    let own_checkout = root.join(".git").exists()
        && git(&root, &["rev-parse", "--show-toplevel"])
            .and_then(|p| Path::new(&p).canonicalize().ok()) == root.canonicalize().ok();
    let commit = own_checkout.then(|| git(&root, &["rev-parse", "--verify", "HEAD"]))
        .flatten().filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let status = if commit.is_some() {
        match git(&root, &["status", "--porcelain", "--untracked-files=normal"]) {
            Some(s) if s.is_empty() => "Clean",
            Some(_) => "Local changes",
            None => "Unknown",
        }
    } else { "Unknown" };
    println!("cargo::rustc-env=PIXELFERRITE_COMMIT={}", commit.as_deref().unwrap_or("Unknown"));
    println!("cargo::rustc-env=PIXELFERRITE_SOURCE_STATUS={status}");
    println!("cargo::rustc-env=PIXELFERRITE_TARGET={}", env::var("TARGET").unwrap());
    println!("cargo::rustc-env=PIXELFERRITE_PROFILE={}", env::var("PROFILE").unwrap());
}
