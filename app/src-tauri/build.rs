fn main() {
    emit_build_metadata();
    // Phase 65 (build reliability): re-run the build script — and thus
    // re-embed the frontend via generate_context! — whenever the built
    // `dist/` changes. Without this, a pure-frontend change (no .rs edit)
    // could leave the OLD frontend embedded in the binary: the symptom
    // was build #5 shipping stale JS (the new wheel diagnostics never
    // appeared). Belt-and-suspenders over tauri_build's own watching.
    println!("cargo:rerun-if-changed=../dist");
    tauri_build::build()
}

// Phase 8.E: emit `WINMUX_GIT_HASH` and `WINMUX_BUILD_TIME` so the dev
// introspection RPC can show what's running. Falls back to "unknown" if git
// isn't available (e.g. building from a tarball).
fn emit_build_metadata() {
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=WINMUX_GIT_HASH={hash}");

    let build_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=WINMUX_BUILD_TIME={build_time}");

    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
