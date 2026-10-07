//! Exercise Cargo's actual incremental-build path, not just Git parsing.
use std::{fs, path::{Path, PathBuf}, process::Command};

struct Scratch(PathBuf);
impl Drop for Scratch { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

fn run(command: &mut Command) -> String {
    let result = command.output().unwrap();
    assert!(result.status.success(), "{command:?}\n{}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

fn git(root: &Path, args: &[&str]) -> String {
    run(Command::new("git").arg("-C").arg(root)
        .args(["-c", "user.name=Build Identity Test", "-c", "user.email=test@example.invalid",
            "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"]).args(args))
}

fn source(root: &Path) {
    let package = root.join("crates/pixelferrite");
    fs::create_dir_all(package.join("src")).unwrap();
    fs::write(package.join("Cargo.toml"), "[package]\nname='identity-fixture'\nversion='0.1.0'\nedition='2024'\n").unwrap();
    fs::write(package.join("build.rs"), include_str!("../build.rs")).unwrap();
    fs::write(package.join("src/main.rs"), r#"fn main() {
        println!("{}\n{}", env!("PIXELFERRITE_COMMIT"), env!("PIXELFERRITE_SOURCE_STATUS"));
    }"#).unwrap();
    // Generate the lockfile before the clean-source assertion.
    run(Command::new(env!("CARGO")).args(["generate-lockfile", "--offline"])
        .arg("--manifest-path").arg(package.join("Cargo.toml")));
}

fn build(root: &Path, target: &Path) -> String {
    run(Command::new(env!("CARGO")).args(["run", "--quiet", "--offline", "--locked"])
        .arg("--manifest-path").arg(root.join("crates/pixelferrite/Cargo.toml"))
        .env("CARGO_TARGET_DIR", target))
}

#[test]
fn incremental_build_tracks_commits_dirty_sources_worktrees_and_archives() {
    let scratch = Scratch(std::env::temp_dir().join(format!("pf-build-identity-{}", std::process::id())));
    fs::create_dir(&scratch.0).unwrap();
    let root = scratch.0.join("checkout");
    let target = scratch.0.join("target");
    source(&root);
    git(&root, &["init", "-q"]);
    fs::write(root.join("core.txt"), "original source outside the app crate").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "Initial fixture"]);
    let first = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(build(&root, &target), format!("{first}\nClean"));

    fs::write(root.join("core.txt"), "modified sibling source").unwrap();
    assert_eq!(build(&root, &target), format!("{first}\nLocal changes"));
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "Changed sibling"]);
    let second = git(&root, &["rev-parse", "HEAD"]);
    assert_ne!(first, second);
    assert_eq!(build(&root, &target), format!("{second}\nClean"));

    git(&root, &["commit", "--allow-empty", "-qm", "Commit without source changes"]);
    let third = git(&root, &["rev-parse", "HEAD"]);
    assert_ne!(second, third);
    assert_eq!(build(&root, &target), format!("{third}\nClean"));

    fs::write(root.join("untracked.txt"), "uncommitted source").unwrap();
    assert_eq!(build(&root, &target), format!("{third}\nLocal changes"));
    fs::remove_file(root.join("untracked.txt")).unwrap();
    assert_eq!(build(&root, &target), format!("{third}\nClean"));

    let linked = scratch.0.join("linked");
    git(&root, &["worktree", "add", "--detach", linked.to_str().unwrap(), &first]);
    assert_eq!(build(&linked, &target), format!("{first}\nClean"));

    let archive = root.join("archive-without-git");
    source(&archive);
    assert_eq!(build(&archive, &target), "Unknown\nUnknown", "an archive must not use its parent's commit");
}
