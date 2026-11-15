//! Build script: enforces the supported target matrix before handing off to tauri-build.
//!
//! Supported: Windows x64 + arm64 (MSVC), macOS Apple Silicon, Linux x64 + arm64 (AppImage).
//! Anything else (32-bit Windows, Intel macOS, ...) fails the build on purpose.

const SUPPORTED_TARGETS: &[&str] = &[
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
];

fn main() {
    let target = std::env::var("TARGET").expect("cargo always sets TARGET for build scripts");
    if !SUPPORTED_TARGETS.contains(&target.as_str()) {
        panic!(
            "unsupported build target `{target}`. Supported targets: {}",
            SUPPORTED_TARGETS.join(", ")
        );
    }
    tauri_build::build();
}
