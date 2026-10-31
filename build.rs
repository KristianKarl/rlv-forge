use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RLV_FORGE_GIT_SHA");
    for name in ["HEAD", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &reference]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let sha = std::env::var("RLV_FORGE_GIT_SHA")
        .ok()
        .filter(|sha| !sha.is_empty())
        .or_else(|| git(&["rev-parse", "--short=7", "HEAD"]))
        .unwrap_or_else(|| {
            println!("cargo:warning=Commit SHA unavailable; set RLV_FORGE_GIT_SHA when building without Git metadata");
            "unknown".to_owned()
        });
    println!("cargo:rustc-env=RLV_FORGE_GIT_SHA={sha}");
}
