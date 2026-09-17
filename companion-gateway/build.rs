//! Bakes the build's revision into the binary, for the version handshake the
//! health endpoint serves: the Companion is installed as a PWA, so a service
//! worker can hold a stale app shell after the Gateway is upgraded; the
//! client compares the Gateway's version with the one baked into its own
//! build and reloads on a mismatch.
//!
//! The revision comes from `TWALK_BUILD_REVISION` when the build environment
//! sets it (the container build does: its context carries no `.git`), from
//! `git describe` otherwise, and is `unknown` when neither is available. It
//! is provenance, not the handshake: the handshake compares the package
//! version, which is always exact.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TWALK_BUILD_REVISION");
    for path in [".git/HEAD", "../.git/HEAD"] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let revision = std::env::var("TWALK_BUILD_REVISION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=TWALK_BUILD_REVISION={revision}");
}

fn git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--always", "--dirty", "--tags"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!revision.is_empty()).then_some(revision)
}
