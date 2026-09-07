//! The diagnostic core behind `--support`: privacy rules (`redact`), the
//! collectors that read the host and its USB tree through injectable roots
//! (`collect`, `inventory`), the bundle writer (`bundle`), and the
//! orchestrator (`support`). Nothing here changes the system; every missing
//! file or failed probe becomes a [`Note`] and the bundle continues.

use serde::{Deserialize, Serialize};

pub mod bundle;
pub mod collect;
pub mod inventory;
pub mod redact;
pub mod support;

/// One "unavailable: reason" record. Collectors return these instead of
/// failing; the manifest lists every one so a reporter and a maintainer both
/// know what the bundle lacks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub item: String,
    pub reason: String,
}

pub fn note(item: &str, reason: impl std::fmt::Display) -> Note {
    Note {
        item: item.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_records_the_item_and_the_reason_as_text() {
        let n = note("dmesg", std::io::Error::other("permission denied"));
        assert_eq!(
            n,
            Note {
                item: "dmesg".to_string(),
                reason: "permission denied".to_string(),
            }
        );
    }
}
