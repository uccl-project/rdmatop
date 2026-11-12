//! Embed the git revision at build time so `rdmatop --version` (and the
//! agent's startup line) can identify the exact source a binary was built
//! from. Falls back to the crate version alone when git is unavailable
//! (e.g. building from a source tarball).

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    let describe = Command::new("git")
        .args(["describe", "--always", "--dirty", "--tags"])
        .output();
    let rev = match describe {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    };
    if !rev.is_empty() {
        println!("cargo:rustc-env=RDMATOP_GIT_REV={rev}");
    }
}
