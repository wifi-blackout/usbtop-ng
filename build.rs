//! Records the compiler version for `--support` and, when the optional
//! `ebpf` feature is enabled, builds the `usbrate` eBPF program into a generated skeleton.
//!
//! Under the default build (feature off) this is a no-op: it returns before
//! touching the filesystem or the `libbpf-cargo`/clang BPF toolchain, so
//! `cargo build` with no features needs neither.

use std::env;

fn main() {
    // Record the compiler for `--support`'s build.toml (`option_env!`
    // reads it back as `USBTOP_NG_RUSTC`). Best-effort: a missing or odd
    // RUSTC just leaves the value unset.
    if let Some(rustc) = env::var_os("RUSTC") {
        if let Ok(output) = std::process::Command::new(rustc).arg("--version").output() {
            if let Ok(text) = String::from_utf8(output.stdout) {
                println!("cargo:rustc-env=USBTOP_NG_RUSTC={}", text.trim());
            }
        }
    }

    // Cargo sets `CARGO_FEATURE_<NAME>` for every enabled feature. Bail out
    // immediately when `ebpf` isn't on so the default build never touches
    // the BPF toolchain.
    if env::var_os("CARGO_FEATURE_EBPF").is_none() {
        return;
    }

    build_skeleton();
}

#[cfg(feature = "ebpf")]
fn build_skeleton() {
    use libbpf_cargo::SkeletonBuilder;
    use std::path::PathBuf;

    const SOURCE: &str = "src/bpf/usbrate.bpf.c";

    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-changed=src/bpf/vmlinux.h");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let skel = out_dir.join("usbrate.skel.rs");

    SkeletonBuilder::new()
        .source(SOURCE)
        .build_and_generate(&skel)
        .expect("failed to build the usbrate eBPF skeleton");
}

// Kept separate from the `CARGO_FEATURE_EBPF` check above (rather than
// relying on it alone) so this file only ever references `libbpf_cargo`
// under `cfg(feature = "ebpf")`: that optional build-dependency isn't even
// compiled into the build script's dependency graph when the feature is
// off, so an unguarded reference would fail to build.
#[cfg(not(feature = "ebpf"))]
fn build_skeleton() {}
