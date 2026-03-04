use std::process::Command;

fn main() {
    // Version from `git describe --tags`, e.g. "v0.2.0" or "v0.2.0-3-g384ac40".
    // Falls back to Cargo.toml version if no tags exist.
    let version = Command::new("git")
        .args(["describe", "--tags", "--always"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let raw = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let raw = raw.strip_prefix('v').unwrap_or(&raw);
            // "0.2.0-3-g384ac40" → "0.2.0-3" (hash is shown separately)
            match raw.rfind("-g") {
                Some(pos) => raw[..pos].to_string(),
                None => raw.to_string(),
            }
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let year = Command::new("git")
        .args(["log", "-1", "--format=%cd", "--date=format:%Y"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "2026".to_string());

    println!("cargo:rustc-env=APP_VERSION={version}");
    println!("cargo:rustc-env=GIT_SHORT_HASH={hash}");
    println!("cargo:rustc-env=GIT_COMMIT_YEAR={year}");
}
