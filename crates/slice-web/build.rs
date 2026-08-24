//! Stamp the build with the commit it came from.
//!
//! The interface shows this, linked to the commit on GitHub, so that "what exactly is
//! deployed?" has an answer anyone can check rather than one that has to be taken on
//! trust. A deployed page is otherwise anonymous: the version string moves once a release,
//! and every push in between looks identical from the outside.
//!
//! Three things this has to get right, because a build stamp that is wrong is worse than
//! no build stamp — it invites someone to read a commit that is not what they are running.
//!
//! * **Staleness.** Cargo caches build scripts, so without telling it what to watch, the
//!   hash would be captured once and then survive every later commit. `rerun-if-changed`
//!   on `HEAD` and on whatever ref it points at covers both a new commit and a branch
//!   switch.
//! * **Uncommitted changes.** A local build from a dirty tree is not the commit it names.
//!   It gets a `+` and the interface declines to link it. Detecting that needs the script
//!   to re-run when *any* tracked file changes, not just when a commit lands -- editing a
//!   source file does not touch `.git/HEAD`, so without watching the files themselves
//!   cargo hands back the previous clean stamp and the page links a modified build to a
//!   commit that does not describe it. That was this mechanism's real failure mode, and
//!   `tools/commit-stamp-check.sh` is what found it.
//! * **No git at all.** A source tarball, or a build in a container without the `.git`
//!   directory. The stamp becomes `unknown` and the interface shows the version alone.
//!
//! In GitHub Actions, `GITHUB_SHA` is what was checked out and is preferred: the runner
//! checks out a detached commit, and asking git there answers the same question more
//! slowly and with more ways to fail.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let commit = from_ci().unwrap_or_else(|| from_git().unwrap_or_else(|| "unknown".into()));
    println!("cargo:rustc-env=SLICE_COMMIT={commit}");

    // Cargo only re-runs this when something it has been told about changes. Without
    // these it would run once and the hash would then be frozen for the life of the
    // target directory.
    if let Some(git) = git_dir() {
        println!("cargo:rerun-if-changed={}", git.join("HEAD").display());
        if let Some(reference) = head_ref(&git) {
            let path = git.join(&reference);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            } else {
                // A packed ref has no file of its own; watching the pack file is the
                // nearest thing, and it changes when refs are packed or updated.
                println!(
                    "cargo:rerun-if-changed={}",
                    git.join("packed-refs").display()
                );
            }
        }
    }
    watch_tracked_files();
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
}

/// Watch every file git tracks, so that editing one re-runs this and the `+` appears.
///
/// The list comes from git rather than from a directory walk because it has to agree with
/// what `git status` calls a modification -- and because a walk would sweep in `target/`,
/// which changes on every build and would leave the script re-running forever.
///
/// A couple of hundred files costs cargo a `stat` each. That is nothing beside the build
/// it precedes, and it buys a stamp that cannot quietly describe the wrong code.
fn watch_tracked_files() {
    // `:/` for the whole repository and `--full-name` for paths relative to its root:
    // this runs in the package directory, and the plain form would list only the files
    // beneath it. See `from_git` for why that scoping is easy to miss.
    let Some(list) = run(&["ls-files", "-z", "--full-name", "--", ":/"]) else {
        return;
    };
    let Some(root) = run(&["rev-parse", "--show-toplevel"]) else {
        return;
    };
    let root = Path::new(&root);
    for file in list.split('\0').filter(|file| !file.is_empty()) {
        println!("cargo:rerun-if-changed={}", root.join(file).display());
    }
}

fn from_ci() -> Option<String> {
    let sha = std::env::var("GITHUB_SHA").ok()?;
    (sha.len() >= 7).then_some(sha)
}

fn from_git() -> Option<String> {
    let head = run(&["rev-parse", "HEAD"])?;

    // `--porcelain` is empty exactly when the tree matches the commit. Untracked files
    // are excluded: they are not part of the build, and a stray note in the working
    // directory should not mark an otherwise faithful build as modified.
    //
    // The `:/` pathspec means "the whole repository". Cargo runs this from the package
    // directory, and git commands scope themselves to where they are run, so without it
    // the question asked would be "has anything under crates/slice-web changed" -- and an
    // edit to the engine in slice-core, which is most of the code, would not count.
    let dirty = run(&["status", "--porcelain", "--untracked-files=no", "--", ":/"])
        .map(|out| !out.is_empty())
        .unwrap_or(false);

    Some(if dirty { format!("{head}+") } else { head })
}

fn run(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

fn git_dir() -> Option<PathBuf> {
    let path = run(&["rev-parse", "--git-dir"])?;
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Some(path)
    } else {
        // Relative to the crate directory, which is where cargo runs this.
        Some(Path::new(&std::env::var("CARGO_MANIFEST_DIR").ok()?).join(path))
    }
}

fn head_ref(git: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git.join("HEAD")).ok()?;
    // "ref: refs/heads/main" on a branch; a bare hash when detached, which has no ref
    // file to watch and cannot move without HEAD itself changing.
    head.strip_prefix("ref: ").map(|r| r.trim().to_string())
}
