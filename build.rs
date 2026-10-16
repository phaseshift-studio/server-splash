use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Record when this binary was built.
///
/// Without it a stale binary is invisible: `--version` reports the same string for
/// every build, so "I deployed and nothing changed" has no obvious cause. The stamp
/// appears in `--version`, in `dashboard.json`, and in the generated page footer,
/// which makes the question answerable from a browser without touching the host.
fn main() {
    let stamp = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%MZ"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("epoch-{secs}")
        });

    println!("cargo:rustc-env=SPLASH_BUILD={stamp}");
    // Deliberately no rerun-if-changed: cargo then re-runs this whenever any file in
    // the package changes, which is exactly when a fresh stamp is wanted.
}
