//! Skeleton for the opt-in eBPF capture backend (feature `ebpf`).
//!
//! `build.rs` compiles `src/bpf/usbrate.bpf.c` (via
//! `libbpf_cargo::SkeletonBuilder`) into a generated `usbrate.skel.rs` in
//! `OUT_DIR`, and this module pulls it into the crate. This only proves the
//! skeleton generates and compiles; loading, attaching, and draining it into
//! `TrafficDelta`s is future work.

include!(concat!(env!("OUT_DIR"), "/usbrate.skel.rs"));

/// Whether this crate was built with the `usbrate` eBPF skeleton compiled
/// in. Always `true` when this module is compiled at all (it only exists
/// under `#[cfg(feature = "ebpf")]`); exists so the module has a real,
/// callable item rather than being include-only.
pub fn ebpf_feature_built() -> bool {
    // Constructing (not opening) a builder touches the skeleton's
    // generated top-level `pub use self::imp::*` -- which libbpf-cargo
    // emits outside the `#[allow(dead_code)]` it wraps around the rest of
    // `mod imp` -- so that re-export isn't flagged unused before real
    // skeleton usage (load/attach) lands in a later task. This performs no
    // I/O and touches no kernel state.
    let _builder = UsbrateSkelBuilder::default();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_the_feature_as_built() {
        assert!(ebpf_feature_built());
    }
}
