//! The `--capture-fixture` subcommand: capture one ladder stage into a
//! committed, hermetic fixture bundle. Feature-gated developer/CI tooling.

pub mod meta;
pub mod sanitize;
pub mod sysfs;
pub mod trace;
