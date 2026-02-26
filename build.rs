use std::process::Command;

fn main() {
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

    println!("cargo:rustc-env=GIT_SHORT_HASH={hash}");
    println!("cargo:rustc-env=GIT_COMMIT_YEAR={year}");
}
