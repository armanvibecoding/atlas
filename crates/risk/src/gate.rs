//! `RiskGate::check(&State, &Intent) -> Decision` — saf fonksiyon, yan etkisiz (§4.5).
//!
//! Riski azaltan niyetler (kapat, iptal) her zaman geçer. Açılış niyetleri sırayla:
//! kill → duraklatma → enstrüman/seans → firma saatleri → hafta sonu → haber → geçerlilik →
//! mevcut maruziyet → cooldown → kontrat tavanı → grup net maruziyet → işlem başı risk →
//! tabana kalan oda → günlük zarar odası → consistency tavanı.

use crate::state::{RiskEnv, RiskState};
use atlas_core::units::NANOS_PER_MIN;
use atlas_core::{
    Action, EntryType, Event, EventKind, Instrument, Intent, Money, Price, Qty, Side,
};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "code", content = "detail")]
pub enum RejectReason {
    Killed(String),
    Paused,
    UnknownInstrument,
    SessionClosed,
    EntriesClosedByRules,
    NearWeekClose,
    NewsBlackout,
    NoPrice,
    BadQty,
    BadStop,
    BadTarget,
    AlreadyExposed,
    Cooldown,
    MaxContracts {
        after_milli: i64,
        max_milli: i64,
    },
    GroupExposure {
        group: String,
        after_milli: i64,
        max_milli: i64,
    },
    RiskPerTrade {
        risk: Money,
        max: Money,
    },
    FloorRoom {
        risk: Money,
        room: Money,
    },
    DailyLossRoom {
        risk: Money,
        room: Money,
    },
    ConsistencyHeadroom {
        today: Money,
        cap: Money,
    },
    CostMissing(String),
    AlgoNotAllowed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Approve,
    Reject(RejectReason),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub verdict: Verdict,
    /// Kararın ürettiği olaylar (ör. `consistency_headroom`). Kapı bunları yaymaz; çağıran yayar.
    pub events: Vec<Event>,
}

impl Decision {
    fn approve() -> Self {
        Decision {
            verdict: Verdict::Approve,
            events: Vec::new(),
        }
    }
    fn reject(r: RejectReason) -> Self {
        Decision {
            verdict: Verdict::Reject(r),
            events: Vec::new(),
        }
    }
    pub fn is_approved(&self) -> bool {
        matches!(self.verdict, Verdict::Approve)
    }
}

/// Risk hesabında kullanılacak giriş fiyatı: gerçekleşebilecek **en kötü** dolum.
///
/// - Piyasa: son fiyat.
/// - Limit: limit fiyatı (dolum bundan kötü olamaz).
/// - Stop: tetiklenmişse piyasadan dolar → alışta `max(stop, son)`, satışta `min(stop, son)`.
///   Son fiyat yoksa `None` (risk sınırlanamaz → red).
pub fn risk_entry_ref(side: Side, entry: EntryType, last: Option<Price>) -> Option<Price> {
    match entry {
        EntryType::Market => last,
        EntryType::Limit(p) => Some(p),
        EntryType::Stop(p) => last.map(|l| match side {
            Side::Buy => p.max(l),
            Side::Sell => p.min(l),
        }),
    }
}

/// Bir girişin stopa kadar en kötü kaybı: mesafe + kayma tamponu, iki yön ücret.
pub fn entry_risk(
    inst: &Instrument,
    side: Side,
    qty: i32,
    entry_ref: Price,
    stop: Price,
    buffer_ticks: i64,
) -> Result<Money, RejectReason> {
    let dist = match side {
        Side::Buy => entry_ref - stop,
        Side::Sell => stop - entry_ref,
    };
    let ticks = (dist + buffer_ticks).max(0);
    let loss = inst.tick_value * ticks.saturating_mul(i64::from(qty));
    let fees = inst
        .fee(entry_ref, Qty(qty))
        .and_then(|a| inst.fee(stop, Qty(qty)).map(|b| a + b))
        .map_err(|e| RejectReason::CostMissing(e.to_string()))?;
    Ok(loss + fees)
}

#[derive(Debug, Clone)]
pub struct RiskGate {
    pub env: RiskEnv,
}

impl RiskGate {
    pub fn new(env: RiskEnv) -> Self {
        RiskGate { env }
    }

    pub fn check(&self, state: &RiskState, intent: &Intent) -> Decision {
        if intent.is_risk_reducing() {
            return Decision::approve();
        }
        match self.check_open(state, intent) {
            Ok(d) => d,
            Err(r) => Decision::reject(r),
        }
    }

    fn check_open(&self, state: &RiskState, intent: &Intent) -> Result<Decision, RejectReason> {
        let env = &self.env;
        let Action::Open {
            side,
            qty,
            entry,
            stop_loss,
            take_profit,
            ..
        } = &intent.action
        else {
            return Ok(Decision::approve());
        };
        let (side, qty, stop) = (*side, *qty, *stop_loss);
        let now = intent.ts.max(state.now);

        if !env.rules.algo_trading_allowed {
            return Err(RejectReason::AlgoNotAllowed);
        }
        if let Some(scope) = state.kills.blocking(&intent.strategy) {
            return Err(RejectReason::Killed(scope.label()));
        }
        if state.paused.contains(&intent.strategy) {
            return Err(RejectReason::Paused);
        }
        let inst = env
            .instruments
            .by_id(intent.instrument)
            .ok_or(RejectReason::UnknownInstrument)?;
        let session = env
            .calendar
            .session(&inst.session)
            .map_err(|_| RejectReason::UnknownInstrument)?;
        if !session.is_open(now) {
            return Err(RejectReason::SessionClosed);
        }
        if env.entries_closed(now) || env.must_be_flat(now) {
            return Err(RejectReason::EntriesClosedByRules);
        }
        if env.near_week_close(
            intent.instrument,
            now,
            env.policy.min_minutes_before_week_close(),
        ) {
            return Err(RejectReason::NearWeekClose);
        }
        if let Some((before, after)) = env.rules.news_blackout_minutes {
            let b = u64::from(before) * NANOS_PER_MIN;
            let a = u64::from(after) * NANOS_PER_MIN;
            if state
                .news
                .iter()
                .any(|n| now.0 >= n.0.saturating_sub(b) && now.0 <= n.0.saturating_add(a))
            {
                return Err(RejectReason::NewsBlackout);
            }
        }
        if qty <= 0 {
            return Err(RejectReason::BadQty);
        }
        let entry_ref = risk_entry_ref(side, *entry, state.last_price(intent.instrument))
            .ok_or(RejectReason::NoPrice)?;
        let stop_ok = match side {
            Side::Buy => stop < entry_ref,
            Side::Sell => stop > entry_ref,
        };
        if !stop_ok {
            return Err(RejectReason::BadStop);
        }
        if let Some(tp) = take_profit {
            let ok = match side {
                Side::Buy => *tp > entry_ref,
                Side::Sell => *tp < entry_ref,
            };
            if !ok {
                return Err(RejectReason::BadTarget);
            }
        }
        if state.has_exposure(&intent.strategy, intent.instrument) {
            return Err(RejectReason::AlreadyExposed);
        }
        if let Some(t) = state.last_stop(&intent.strategy, intent.instrument, side) {
            let cd = u64::from(env.policy.cooldown_after_stop_min()) * NANOS_PER_MIN;
            if now.since(t) < cd {
                return Err(RejectReason::Cooldown);
            }
        }
        let w = inst.exposure_weight_milli;
        let after = state.weighted_contracts_milli(env) + i64::from(qty) * w;
        if after > env.rules.max_contracts_milli {
            return Err(RejectReason::MaxContracts {
                after_milli: after,
                max_milli: env.rules.max_contracts_milli,
            });
        }
        if let Some(group) = &inst.exposure_group {
            let after = state.group_net_milli(env, group) + i64::from(qty * side.sign()) * w;
            if after.abs() > env.policy.max_group_net_milli() {
                return Err(RejectReason::GroupExposure {
                    group: group.clone(),
                    after_milli: after,
                    max_milli: env.policy.max_group_net_milli(),
                });
            }
        }
        let buf = env.policy.stop_slippage_buffer_ticks();
        let risk = entry_risk(inst, side, qty, entry_ref, stop, buf)?;
        if risk > env.policy.max_risk_per_trade() {
            return Err(RejectReason::RiskPerTrade {
                risk,
                max: env.policy.max_risk_per_trade(),
            });
        }
        let committed = state.committed_risk(env)?;
        let equity = state.equity(env);
        let room = equity - state.floor() - committed;
        if risk >= room {
            return Err(RejectReason::FloorRoom { risk, room });
        }
        if let Some(limit) = env.rules.daily_loss_limit {
            let reference = match env.rules.daily_loss_reference {
                crate::config::DailyLossReference::DayStartBalance => state.day_start_balance(),
                crate::config::DailyLossReference::DayStartEquity => state.day_start_equity(),
            };
            let used = (reference - equity).max(Money::ZERO);
            let room = limit - used - committed;
            if risk >= room {
                return Err(RejectReason::DailyLossRoom { risk, room });
            }
        }
        if let Some(cap) = state.consistency_cap_today(env) {
            let today = state.today_profit(env);
            let threshold = Money(
                i64::try_from(
                    i128::from(cap.0) * i128::from(env.policy.consistency_headroom_milli()) / 1000,
                )
                .unwrap_or(i64::MAX),
            );
            if today >= threshold {
                let mut d = Decision::reject(RejectReason::ConsistencyHeadroom { today, cap });
                d.events.push(
                    Event::new(
                        now,
                        EventKind::ConsistencyHeadroom,
                        serde_json::json!({"today_profit": today, "cap_today": cap, "threshold": threshold}),
                    )
                    .with_key(format!("consistency:{}", env.day_id(now))),
                );
                return Ok(d);
            }
        }
        Ok(Decision::approve())
    }
}
