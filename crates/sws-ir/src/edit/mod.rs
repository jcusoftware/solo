//! Canonical editor command reducer: stable node IDs, versioned command
//! batches, atomic apply, deterministic inverses, undo/redo history,
//! touched-field reporting and the lossless-save check.
//!
//! Pure and `wasm32`-clean (no I/O). See `docs/EDITOR_COMMANDS.md`.

pub mod api;
pub mod command;
pub mod history;
pub mod ids;
pub mod lossless;
pub mod reducer;

#[cfg(test)]
mod tests;

pub use api::{
    EditOptions, EditOutput, UpgradeOutput, apply_batch_json, canonical_json, parse_batch,
    upgrade_json,
};
pub use command::{
    COMMAND_BATCH_VERSION, Command, CommandBatch, CommandError, ContainerRef, ErrorCode,
    MAX_BATCH_COMMANDS, Transaction, paths_conflict, touched_conflict,
};
pub use history::{DEFAULT_HISTORY_LIMIT, History};
pub use ids::{
    AssignedId, MARKER_KEY, UID_SCHEME, UpgradeReport, assign_stable_ids, check_editable,
    check_uids, is_valid_uid,
};
pub use lossless::{check_lossless, lossy_paths};
pub use reducer::{apply_batch, apply_batch_in_place};
