//! Günlük LLM token/USD bütçesi (§4.5, §4.8). Bütçe biterse ajanlar durur, sistem mekanik çalışır.

use atlas_core::{Event, EventKind, Money, UnixNanos};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmBudget {
    pub daily_usd: Money,
    pub daily_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetTracker {
    limit: LlmBudget,
    day: Option<NaiveDate>,
    spent_usd: Money,
    spent_tokens: u64,
    exhausted_reported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "LLM bütçesi tükendi ({day}): {spent_usd} / {limit_usd}, {spent_tokens} / {limit_tokens} token"
)]
pub struct BudgetExhausted {
    pub day: String,
    pub spent_usd: Money,
    pub limit_usd: Money,
    pub spent_tokens: u64,
    pub limit_tokens: u64,
}

impl BudgetTracker {
    pub fn new(limit: LlmBudget) -> Self {
        BudgetTracker {
            limit,
            day: None,
            spent_usd: Money::ZERO,
            spent_tokens: 0,
            exhausted_reported: false,
        }
    }

    fn roll(&mut self, day: NaiveDate) {
        if self.day != Some(day) {
            self.day = Some(day);
            self.spent_usd = Money::ZERO;
            self.spent_tokens = 0;
            self.exhausted_reported = false;
        }
    }

    fn exhausted(&self) -> bool {
        self.spent_usd >= self.limit.daily_usd || self.spent_tokens >= self.limit.daily_tokens
    }

    /// Çağrıdan önce: izin var mı?
    pub fn can_spend(&mut self, day: NaiveDate) -> Result<(), BudgetExhausted> {
        self.roll(day);
        if self.exhausted() {
            Err(self.err(day))
        } else {
            Ok(())
        }
    }

    fn err(&self, day: NaiveDate) -> BudgetExhausted {
        BudgetExhausted {
            day: day.to_string(),
            spent_usd: self.spent_usd,
            limit_usd: self.limit.daily_usd,
            spent_tokens: self.spent_tokens,
            limit_tokens: self.limit.daily_tokens,
        }
    }

    /// Çağrıdan sonra: harcamayı düş. Tükenme ilk kez olursa `budget_exhausted` olayı döner.
    pub fn charge(
        &mut self,
        day: NaiveDate,
        ts: UnixNanos,
        usd: Money,
        tokens: u64,
    ) -> Option<Event> {
        self.roll(day);
        self.spent_usd += usd;
        self.spent_tokens = self.spent_tokens.saturating_add(tokens);
        if self.exhausted() && !self.exhausted_reported {
            self.exhausted_reported = true;
            let e = self.err(day);
            return Some(
                Event::new(
                    ts,
                    EventKind::BudgetExhausted,
                    serde_json::json!({"day": e.day, "spent_usd": e.spent_usd, "limit_usd": e.limit_usd,
                        "spent_tokens": e.spent_tokens, "limit_tokens": e.limit_tokens}),
                )
                .with_key(format!("budget:{day}")),
            );
        }
        None
    }

    pub fn spent(&self) -> (Money, u64) {
        (self.spent_usd, self.spent_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausts_once_and_rolls_daily() {
        let d1 = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let mut b = BudgetTracker::new(LlmBudget {
            daily_usd: Money::from_usd(5),
            daily_tokens: 1000,
        });
        assert!(b.can_spend(d1).is_ok());
        assert!(
            b.charge(d1, UnixNanos(1), Money::from_usd(3), 100)
                .is_none()
        );
        assert!(
            b.charge(d1, UnixNanos(2), Money::from_usd(3), 100)
                .is_some()
        );
        assert!(b.charge(d1, UnixNanos(3), Money::from_usd(1), 1).is_none());
        assert!(b.can_spend(d1).is_err());
        assert!(b.can_spend(d2).is_ok());
        assert_eq!(b.spent(), (Money::ZERO, 0));
        assert!(b.charge(d2, UnixNanos(4), Money::ZERO, 1000).is_some());
    }
}
