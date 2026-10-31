//! Kill switch'ler: global, hesap, strateji. Tetiklenince açık pozisyonlar kapanır ve
//! yeniden açma yalnız manuel (`kill-reset`, Arman) ile olur.

use atlas_core::{Event, EventKind, StrategyId, UnixNanos};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope", content = "id")]
pub enum KillScope {
    Global,
    Account,
    Strategy(StrategyId),
}

impl KillScope {
    pub fn parse(s: &str) -> Result<KillScope, String> {
        match s.trim() {
            "global" => Ok(KillScope::Global),
            "account" => Ok(KillScope::Account),
            other => match other.strip_prefix("strategy:") {
                Some(id) if !id.is_empty() => Ok(KillScope::Strategy(StrategyId::new(id))),
                _ => Err(format!(
                    "kapsam global | account | strategy:<id> olmalı, {other:?}"
                )),
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            KillScope::Global => "global".into(),
            KillScope::Account => "account".into(),
            KillScope::Strategy(s) => format!("strategy:{s}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillRecord {
    pub ts: UnixNanos,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillState {
    global: Option<KillRecord>,
    account: Option<KillRecord>,
    strategies: BTreeMap<StrategyId, KillRecord>,
}

impl KillState {
    /// Tetikler. Zaten tetikliyse yeni olay üretmez (dedupe) ve `None` döner.
    pub fn trigger(&mut self, scope: KillScope, reason: &str, ts: UnixNanos) -> Option<Event> {
        let rec = KillRecord {
            ts,
            reason: reason.to_string(),
        };
        let fresh = match &scope {
            KillScope::Global => self.global.replace(rec).is_none(),
            KillScope::Account => self.account.replace(rec).is_none(),
            KillScope::Strategy(s) => self.strategies.insert(s.clone(), rec).is_none(),
        };
        fresh.then(|| {
            Event::new(
                ts,
                EventKind::KillSwitch,
                serde_json::json!({ "scope": scope.label(), "reason": reason }),
            )
            .with_key(format!("kill:{}", scope.label()))
        })
    }

    /// Manuel açma. Yalnız Arman'ın `kill-reset` komutu çağırır.
    pub fn reset(&mut self, scope: &KillScope, ts: UnixNanos) -> Option<Event> {
        let was = match scope {
            KillScope::Global => self.global.take().is_some(),
            KillScope::Account => self.account.take().is_some(),
            KillScope::Strategy(s) => self.strategies.remove(s).is_some(),
        };
        was.then(|| {
            Event::new(
                ts,
                EventKind::KillReset,
                serde_json::json!({ "scope": scope.label() }),
            )
        })
    }

    /// Bu stratejiyi engelleyen en geniş kapsam.
    pub fn blocking(&self, strategy: &StrategyId) -> Option<KillScope> {
        if self.global.is_some() {
            Some(KillScope::Global)
        } else if self.account.is_some() {
            Some(KillScope::Account)
        } else if self.strategies.contains_key(strategy) {
            Some(KillScope::Strategy(strategy.clone()))
        } else {
            None
        }
    }

    pub fn any_account_level(&self) -> bool {
        self.global.is_some() || self.account.is_some()
    }

    pub fn active(&self) -> Vec<(KillScope, KillRecord)> {
        let mut v = Vec::new();
        if let Some(r) = &self.global {
            v.push((KillScope::Global, r.clone()));
        }
        if let Some(r) = &self.account {
            v.push((KillScope::Account, r.clone()));
        }
        for (s, r) in &self.strategies {
            v.push((KillScope::Strategy(s.clone()), r.clone()));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_dedupes_and_reset_requires_trigger() {
        let mut k = KillState::default();
        let s = StrategyId::new("a");
        assert!(k.blocking(&s).is_none());
        assert!(
            k.trigger(KillScope::Strategy(s.clone()), "x", UnixNanos(1))
                .is_some()
        );
        assert!(
            k.trigger(KillScope::Strategy(s.clone()), "x", UnixNanos(2))
                .is_none()
        );
        assert_eq!(k.blocking(&s), Some(KillScope::Strategy(s.clone())));
        assert!(k.trigger(KillScope::Global, "y", UnixNanos(3)).is_some());
        assert_eq!(k.blocking(&s), Some(KillScope::Global));
        assert!(k.reset(&KillScope::Global, UnixNanos(4)).is_some());
        assert!(k.reset(&KillScope::Global, UnixNanos(5)).is_none());
        assert_eq!(k.active().len(), 1);
        assert!(k.trigger(KillScope::Account, "z", UnixNanos(6)).is_some());
        assert!(k.any_account_level());
    }

    #[test]
    fn parse_scope() {
        assert_eq!(KillScope::parse("global"), Ok(KillScope::Global));
        assert_eq!(
            KillScope::parse("strategy:abc"),
            Ok(KillScope::Strategy(StrategyId::new("abc")))
        );
        assert!(KillScope::parse("strategy:").is_err());
        assert!(KillScope::parse("everything").is_err());
    }
}
