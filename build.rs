//! Records which commit a binary was built from, for `weir version`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=WEIR_BUILD_COMMIT");
    println!("cargo:rerun-if-changed=build.rs");
    let commit = std::env::var("WEIR_BUILD_COMMIT")
        .ok()
        .filter(|c| !c.is_empty())
        .or_else(|| {
            let out = Command::new("git")
                .args(["rev-parse", "--short=9", "HEAD"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=WEIR_COMMIT={commit}");
}
