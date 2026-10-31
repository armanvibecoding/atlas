//! `risk` — anayasa (kural 5). Yalnız `atlas-core`'a bağlıdır.
//!
//! - Her emir [`RiskGate::check`]'ten geçer.
//! - Firma kuralları yüklendikten sonra değiştirilemez; Atlas politikası yalnız sıkılaştırılabilir.
//! - Kill switch'ler manuel açılır.

pub mod budget;
pub mod config;
pub mod gate;
pub mod kill;
pub mod state;

pub use budget::{BudgetTracker, LlmBudget};
pub use config::{ConfigError, DrawdownType, PropRules, RiskPolicy, TightenOp};
pub use gate::{Decision, RejectReason, RiskGate, Verdict, entry_risk, risk_entry_ref};
pub use kill::{KillScope, KillState};
pub use state::{OpenPosition, PendingEntry, RiskEnv, RiskState, WEEKEND_FLATTEN_LEAD_MIN};

#[cfg(test)]
mod tests;
