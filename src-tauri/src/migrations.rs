//! Application migration entry points, in their original execution phases.
//! Data-specific schema/SQLite versions and native installer migration stay with
//! their owners; there is deliberately no additional migration state file.
use crate::storage::Result;
use std::path::Path;

/// Recover interrupted transactions before loading their final store.
pub(crate) fn gateway(data: &Path) -> Result<(crate::gateway::Store, String)> {
    crate::gateway::recover_transactions(data)?;
    crate::gateway::Store::load(&data.join("gateway.json"))
}

/// Antigravity accounts are OAuth logins only. Before the account store is
/// first read, hand back the app settings a retired Vertex API Key had taken
/// over, then drop the retired ADC and key entries. A migrated store is left
/// untouched, so this is safe to run on every start.
pub(crate) fn antigravity_accounts(paths: &crate::antigravity::auth::Paths) -> Result<()> {
    crate::antigravity::auth::migrate_retired_accounts(paths).map(|_| ())
}

#[cfg(test)]
mod tests;
