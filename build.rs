use std::process::Command;

fn main() {
    // Git short hash — ties every binary to its exact source commit
    let build_id = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=SETU_BUILD_ID={build_id}");
    // Only re-run when the HEAD commit changes (not on every file edit)
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");

    // Embed icon.ico into the Windows executable (Explorer, taskbar, etc.)
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("failed to compile Windows resources");
    }
}
