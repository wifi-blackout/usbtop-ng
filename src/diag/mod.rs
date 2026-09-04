//! The diagnostic core behind `--support`: privacy rules (`redact`), the
//! collectors that read the host and its USB tree through injectable roots
//! (`collect`, `inventory`), the bundle writer (`bundle`), and the
//! orchestrator (`support`). Nothing here changes the system; every missing
//! file or failed probe becomes a [`Note`] and the bundle continues.

use serde::Serialize;

pub mod redact;

/// One "unavailable: reason" record. Collectors return these instead of
/// failing; the manifest lists every one so a reporter and a maintainer both
/// know what the bundle lacks.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[expect(dead_code)]
pub struct Note {
    pub item: String,
    pub reason: String,
}

#[expect(dead_code)]
pub fn note(item: &str, reason: impl std::fmt::Display) -> Note {
    Note {
        item: item.to_string(),
        reason: reason.to_string(),
    }
}
