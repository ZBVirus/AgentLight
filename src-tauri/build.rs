fn main() {
    tauri_build::build();

    // A build identifier so the app can show which build it is. Use an explicit
    // `AGENTLIGHT_BUILD_ID` when the build sets one, else the git short SHA, else
    // "dev". The value is compiled into the binary via `rustc-env`.
    let id = std::env::var("AGENTLIGHT_BUILD_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(git_short_sha)
        .unwrap_or_else(|| "dev".to_string());
    println!("cargo:rustc-env=AGENTLIGHT_BUILD_ID={id}");
    println!("cargo:rerun-if-env-changed=AGENTLIGHT_BUILD_ID");
}

fn git_short_sha() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}
