//! Hesap durumu (`RiskState`): bakiye, pozisyonlar, drawdown tabanı, gün sınırı, kill switch.
//!
//! Durum yalnız dolum ve fiyat olaylarıyla ilerler; aynı olay dizisi aynı durumu üretir
//! (journal replay bunu doğrular).

use crate::config::{DailyLossReference, DrawdownType, LocalMinute, PropRules, RiskPolicy};
use crate::kill::{KillScope, KillState};
use atlas_core::calendar::{Calendar, to_utc};
use atlas_core::{
    Event, EventKind, Fill, FillReason, InstrumentId, InstrumentTable, Money, Price, Side,
    StrategyId, UnixNanos,
};
use chrono::{NaiveDate, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Değişmez risk ortamı: firma kuralları, Atlas politikası, enstrümanlar, takvim.
#[derive(Debug, Clone)]
pub struct RiskEnv {
    pub rules: PropRules,
    pub policy: RiskPolicy,
    pub instruments: InstrumentTable,
    pub calendar: Calendar,
}

impl RiskEnv {
    /// Firmanın gün kimliği: yerel saatten sıfırlama saati çıkarılınca kalan tarih.
    pub fn day_id(&self, ts: UnixNanos) -> NaiveDate {
        let local = to_utc(ts)
            .with_timezone(&self.rules.day_reset_tz)
            .naive_local();
        let shifted = local - chrono::Duration::minutes(i64::from(self.rules.day_reset_time.0));
        shifted.date()
    }

    fn rules_local_minute(&self, ts: UnixNanos) -> u32 {
        let l = to_utc(ts).with_timezone(&self.rules.rules_tz).naive_local();
        l.time().num_seconds_from_midnight() / 60
    }

    /// `[start, firma gün sıfırlaması)` penceresinde mi?
    fn in_window_until_reset(&self, ts: UnixNanos, start: LocalMinute) -> bool {
        let tod = self.rules_local_minute(ts);
        // Sıfırlama saatini kural saat dilimine çevir (genelde aynı dilim).
        let reset_local = {
            let day = self.day_id(ts) + chrono::Duration::days(1);
            let reset_ts = atlas_core::calendar::local_to_utc(
                day,
                self.rules.day_reset_time.0,
                self.rules.day_reset_tz,
            );
            self.rules_local_minute(reset_ts)
        };
        let s = start.0;
        if s == reset_local {
            return false;
        }
        if s < reset_local {
            tod >= s && tod < reset_local
        } else {
            tod >= s || tod < reset_local
        }
    }

    /// Yeni giriş yasak saatinde mi (`no_new_entries_after` → gün sıfırlaması)?
    pub fn entries_closed(&self, ts: UnixNanos) -> bool {
        self.rules
            .no_new_entries_after
            .is_some_and(|m| self.in_window_until_reset(ts, m))
    }

    /// Pozisyonsuz olunması gereken saatte mi (`flat_by` → gün sıfırlaması)?
    pub fn must_be_flat(&self, ts: UnixNanos) -> bool {
        self.rules
            .flat_by
            .is_some_and(|m| self.in_window_until_reset(ts, m))
    }

    /// Hafta sonu tutma yasaksa, enstrümanın haftalık kapanışına kalan süre `lead_min`'den az mı?
    pub fn near_week_close(&self, inst: InstrumentId, ts: UnixNanos, lead_min: u32) -> bool {
        if self.rules.weekend_holding_allowed {
            return false;
        }
        let Some(i) = self.instruments.by_id(inst) else {
            return false;
        };
        let Ok(s) = self.calendar.session(&i.session) else {
            return false;
        };
        match s.next_week_close(ts) {
            Some(c) => c.since(ts) <= u64::from(lead_min) * atlas_core::units::NANOS_PER_MIN,
            None => false,
        }
    }
}

/// Hafta kapanışından bu kadar dakika önce pozisyonlar kapatılır (hafta sonu tutma yasaksa).
pub const WEEKEND_FLATTEN_LEAD_MIN: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPosition {
    pub side: Side,
    /// Pozitif adet.
    pub qty: i32,
    pub entry: Price,
    pub stop: Price,
    pub opened: UnixNanos,
}

impl OpenPosition {
    pub fn signed(&self) -> i32 {
        self.qty.saturating_mul(self.side.sign())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEntry {
    pub side: Side,
    pub qty: i32,
    /// Giriş referans fiyatı (limit/stop fiyatı ya da piyasa için son fiyat).
    pub entry_ref: Price,
    pub stop: Price,
}

pub type PosKey = (StrategyId, InstrumentId);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskState {
    pub now: UnixNanos,
    pub balance: Money,
    positions: BTreeMap<PosKey, OpenPosition>,
    pending: BTreeMap<PosKey, PendingEntry>,
    last_price: BTreeMap<InstrumentId, Price>,
    hwm: Money,
    floor: Money,
    day: Option<NaiveDate>,
    day_start_balance: Money,
    day_start_equity: Money,
    /// Kapanmış günlerin kârı (equity farkı).
    daily_profit: BTreeMap<NaiveDate, Money>,
    last_stop: BTreeMap<(StrategyId, InstrumentId, Side), UnixNanos>,
    pub kills: KillState,
    /// Duraklatılmış stratejiler (yalnız kısıtlayıcı; Grok `strategy pause`).
    pub paused: BTreeSet<StrategyId>,
    /// Yüksek etkili haber zamanları (ekonomik takvim).
    pub news: Vec<UnixNanos>,
    near_alerted: Option<NaiveDate>,
    /// Taban ihlali bir kez raporlanır (kill zaten tetikli olsa bile).
    breached: bool,
    /// Günlük zarar limiti o gün için bir kez raporlanır.
    daily_limit_hit: Option<NaiveDate>,
}

impl RiskState {
    pub fn new(env: &RiskEnv, now: UnixNanos) -> Self {
        let start = env.rules.starting_balance;
        let floor = start - env.rules.drawdown_amount;
        RiskState {
            now,
            balance: start,
            positions: BTreeMap::new(),
            pending: BTreeMap::new(),
            last_price: BTreeMap::new(),
            hwm: start,
            floor,
            day: Some(env.day_id(now)),
            day_start_balance: start,
            day_start_equity: start,
            daily_profit: BTreeMap::new(),
            last_stop: BTreeMap::new(),
            kills: KillState::default(),
            paused: BTreeSet::new(),
            news: Vec::new(),
            near_alerted: None,
            breached: false,
            daily_limit_hit: None,
        }
    }

    pub fn positions(&self) -> &BTreeMap<PosKey, OpenPosition> {
        &self.positions
    }
    pub fn pending(&self) -> &BTreeMap<PosKey, PendingEntry> {
        &self.pending
    }
    pub fn last_price(&self, inst: InstrumentId) -> Option<Price> {
        self.last_price.get(&inst).copied()
    }
    pub fn floor(&self) -> Money {
        self.floor
    }
    pub fn high_water_mark(&self) -> Money {
        self.hwm
    }
    pub fn day_start_balance(&self) -> Money {
        self.day_start_balance
    }
    pub fn day_start_equity(&self) -> Money {
        self.day_start_equity
    }
    pub fn daily_profit(&self) -> &BTreeMap<NaiveDate, Money> {
        &self.daily_profit
    }
    pub fn last_stop(&self, s: &StrategyId, i: InstrumentId, side: Side) -> Option<UnixNanos> {
        self.last_stop.get(&(s.clone(), i, side)).copied()
    }

    pub fn unrealized(&self, env: &RiskEnv) -> Money {
        self.positions
            .iter()
            .filter_map(|((_, inst), p)| {
                let px = self.last_price.get(inst)?;
                let i = env.instruments.by_id(*inst)?;
                Some(i.pnl(p.entry, *px, atlas_core::Qty(p.signed())))
            })
            .sum()
    }

    pub fn equity(&self, env: &RiskEnv) -> Money {
        self.balance + self.unrealized(env)
    }

    /// Bugünkü (firma günü) kâr: equity − gün başı bakiye.
    pub fn today_profit(&self, env: &RiskEnv) -> Money {
        self.equity(env) - self.day_start_balance
    }

    /// Bugünden önceki toplam kâr.
    pub fn prior_profit(&self, env: &RiskEnv) -> Money {
        self.day_start_balance - env.rules.starting_balance
    }

    /// Consistency kuralına göre bugünkü kâr tavanı (§4.5, ileriye dönük).
    ///
    /// Bugün `p` kârla kapanırsa toplam `P + p` olur; kural `p ≤ r(P + p)` ⇒ `p ≤ rP/(1−r)`.
    /// `P ≤ 0` iken tavan tanımsızdır (sonraki günler seyreltebilir) → `None`.
    pub fn consistency_cap_today(&self, env: &RiskEnv) -> Option<Money> {
        let r = env.rules.consistency_ratio_milli?;
        let p = self.prior_profit(env);
        if p.0 <= 0 {
            return None;
        }
        let v = i128::from(p.0) * i128::from(r) / i128::from(1000 - r);
        Some(Money(i64::try_from(v).unwrap_or(i64::MAX)))
    }

    /// Kapanmış günlerle consistency kuralı sağlanıyor mu (kontrol).
    pub fn consistency_ok(&self, env: &RiskEnv) -> bool {
        let Some(r) = env.rules.consistency_ratio_milli else {
            return true;
        };
        let total: Money = self.daily_profit.values().copied().sum();
        if total.0 <= 0 {
            return true;
        }
        let max_day = self
            .daily_profit
            .values()
            .copied()
            .max()
            .unwrap_or(Money::ZERO);
        i128::from(max_day.0) * 1000 <= i128::from(total.0) * i128::from(r)
    }

    pub fn has_exposure(&self, s: &StrategyId, i: InstrumentId) -> bool {
        let k = (s.clone(), i);
        self.positions.contains_key(&k) || self.pending.contains_key(&k)
    }

    /// Onaylanmış bir girişi bekleyen olarak kaydet (motor çağırır).
    pub fn register_pending(&mut self, s: StrategyId, i: InstrumentId, p: PendingEntry) {
        self.pending.insert((s, i), p);
    }

    pub fn cancel_pending(&mut self, s: &StrategyId, i: InstrumentId) -> bool {
        self.pending.remove(&(s.clone(), i)).is_some()
    }

    /// Stopu güncelle (izleyen stop). Yalnız riski azaltan yönde kabul edilir.
    pub fn tighten_stop(&mut self, s: &StrategyId, i: InstrumentId, new_stop: Price) -> bool {
        match self.positions.get_mut(&(s.clone(), i)) {
            Some(p) => {
                let tighter = match p.side {
                    Side::Buy => new_stop > p.stop,
                    Side::Sell => new_stop < p.stop,
                };
                if tighter {
                    p.stop = new_stop;
                }
                tighter
            }
            None => false,
        }
    }

    pub fn on_price(
        &mut self,
        env: &RiskEnv,
        ts: UnixNanos,
        inst: InstrumentId,
        price: Price,
    ) -> Vec<Event> {
        self.last_price.insert(inst, price);
        self.evaluate(env, ts)
    }

    pub fn on_fill(&mut self, env: &RiskEnv, fill: &Fill) -> Vec<Event> {
        let mut ev = Vec::new();
        let key = (fill.strategy.clone(), fill.instrument);
        self.last_price.insert(fill.instrument, fill.price);
        let Some(inst) = env.instruments.by_id(fill.instrument) else {
            ev.push(Event::new(
                fill.ts,
                EventKind::EngineFailure,
                serde_json::json!({"error": "dolumda bilinmeyen enstrüman", "instrument": fill.instrument.0}),
            ));
            return ev;
        };
        if fill.reason == FillReason::Entry {
            let stop = self
                .pending
                .remove(&key)
                .map(|p| p.stop)
                .unwrap_or(fill.price);
            self.balance -= fill.fee;
            self.positions.insert(
                key,
                OpenPosition {
                    side: fill.side,
                    qty: fill.qty,
                    entry: fill.price,
                    stop,
                    opened: fill.ts,
                },
            );
            ev.push(Event::new(
                fill.ts,
                EventKind::PositionOpened,
                serde_json::json!({
                    "strategy": fill.strategy, "instrument": inst.symbol, "side": fill.side,
                    "qty": fill.qty, "price": fill.price, "stop": stop, "fee": fill.fee,
                }),
            ));
        } else if let Some(pos) = self.positions.remove(&key) {
            let realized = inst.pnl(pos.entry, fill.price, atlas_core::Qty(pos.signed()));
            self.balance += realized;
            self.balance -= fill.fee;
            if fill.reason == FillReason::StopLoss {
                self.last_stop
                    .insert((fill.strategy.clone(), fill.instrument, pos.side), fill.ts);
            }
            ev.push(Event::new(
                fill.ts,
                EventKind::PositionClosed,
                serde_json::json!({
                    "strategy": fill.strategy, "instrument": inst.symbol, "side": pos.side,
                    "qty": pos.qty, "entry": pos.entry, "exit": fill.price, "reason": fill.reason,
                    "gross": realized, "fee": fill.fee,
                }),
            ));
        } else {
            ev.push(Event::new(
                fill.ts,
                EventKind::EngineFailure,
                serde_json::json!({"error": "pozisyonsuz çıkış dolumu", "strategy": fill.strategy}),
            ));
        }
        ev.extend(self.evaluate(env, fill.ts));
        ev
    }

    fn recompute_floor(&mut self, env: &RiskEnv) {
        let dd = env.rules.drawdown_amount;
        let candidate = match env.rules.drawdown_type {
            DrawdownType::Static => env.rules.starting_balance - dd,
            DrawdownType::EodTrailing | DrawdownType::IntradayTrailing => {
                let c = self.hwm - dd;
                match env.rules.drawdown_lock_level {
                    Some(lock) => c.min(lock),
                    None => c,
                }
            }
        };
        self.floor = self.floor.max(candidate);
    }

    /// Gün değişimi, HWM/taban, ihlal kontrolleri.
    pub fn evaluate(&mut self, env: &RiskEnv, ts: UnixNanos) -> Vec<Event> {
        let mut ev = Vec::new();
        if ts > self.now {
            self.now = ts;
        }
        let equity = self.equity(env);
        let today = env.day_id(self.now);
        if self.day != Some(today) {
            if let Some(prev) = self.day {
                let profit = equity - self.day_start_balance;
                self.daily_profit.insert(prev, profit);
                if env.rules.drawdown_type == DrawdownType::EodTrailing {
                    self.hwm = self.hwm.max(equity);
                }
                ev.push(Event::new(
                    self.now,
                    EventKind::DayRollover,
                    serde_json::json!({"closed_day": prev.to_string(), "profit": profit, "equity": equity}),
                ));
            }
            self.day = Some(today);
            self.day_start_balance = self.balance;
            self.day_start_equity = equity;
        }
        if env.rules.drawdown_type == DrawdownType::IntradayTrailing {
            self.hwm = self.hwm.max(equity);
        }
        self.recompute_floor(env);

        if equity <= self.floor && !self.breached {
            self.breached = true;
            ev.push(Event::new(
                self.now,
                EventKind::RiskBreach,
                serde_json::json!({"equity": equity, "floor": self.floor}),
            ));
            ev.extend(self.kills.trigger(
                KillScope::Account,
                &format!(
                    "drawdown tabanı ihlali: equity {equity} ≤ taban {}",
                    self.floor
                ),
                self.now,
            ));
        }
        if let Some(limit) = env.rules.daily_loss_limit {
            let reference = match env.rules.daily_loss_reference {
                DailyLossReference::DayStartBalance => self.day_start_balance,
                DailyLossReference::DayStartEquity => self.day_start_equity,
            };
            let loss = reference - equity;
            if loss >= limit && self.daily_limit_hit != Some(today) {
                self.daily_limit_hit = Some(today);
                ev.push(Event::new(
                    self.now,
                    EventKind::DailyLossLimit,
                    serde_json::json!({"loss": loss, "limit": limit, "day": today.to_string()}),
                ));
                ev.extend(self.kills.trigger(
                    KillScope::Account,
                    &format!("günlük zarar limiti: {loss} ≥ {limit}"),
                    self.now,
                ));
            }
        }
        let dd = env.rules.drawdown_amount;
        let near = Money(
            i64::try_from(i128::from(dd.0) * i128::from(env.policy.drawdown_near_milli()) / 1000)
                .unwrap_or(i64::MAX),
        );
        if equity - self.floor <= near && self.near_alerted != Some(today) {
            self.near_alerted = Some(today);
            ev.push(
                Event::new(
                    self.now,
                    EventKind::PropDrawdownNear,
                    serde_json::json!({"equity": equity, "floor": self.floor, "distance": equity - self.floor}),
                )
                .with_key(format!("dd_near:{today}")),
            );
        }
        ev
    }

    /// Kalan oda hesapları için: açık pozisyon ve bekleyen girişlerin stopa kadar en kötü kaybı.
    pub fn committed_risk(&self, env: &RiskEnv) -> Result<Money, crate::gate::RejectReason> {
        let buf = env.policy.stop_slippage_buffer_ticks();
        let mut total = Money::ZERO;
        for ((_, inst_id), p) in &self.positions {
            let inst = env
                .instruments
                .by_id(*inst_id)
                .ok_or(crate::gate::RejectReason::UnknownInstrument)?;
            let mark = self.last_price.get(inst_id).copied().unwrap_or(p.entry);
            let dist = match p.side {
                Side::Buy => mark - p.stop,
                Side::Sell => p.stop - mark,
            };
            let ticks = (dist + buf).max(0);
            let loss = inst.tick_value * (ticks.saturating_mul(i64::from(p.qty)));
            let fee = inst
                .fee(p.stop, atlas_core::Qty(p.qty))
                .map_err(|e| crate::gate::RejectReason::CostMissing(e.to_string()))?;
            total += loss + fee;
        }
        for ((_, inst_id), p) in &self.pending {
            let inst = env
                .instruments
                .by_id(*inst_id)
                .ok_or(crate::gate::RejectReason::UnknownInstrument)?;
            total += crate::gate::entry_risk(inst, p.side, p.qty, p.entry_ref, p.stop, buf)?;
        }
        Ok(total)
    }

    /// Ağırlıklı toplam kontrat (binde), pozisyon + bekleyen.
    pub fn weighted_contracts_milli(&self, env: &RiskEnv) -> i64 {
        let w = |i: &InstrumentId| {
            env.instruments
                .by_id(*i)
                .map_or(1000, |x| x.exposure_weight_milli)
        };
        let a: i64 = self
            .positions
            .iter()
            .map(|((_, i), p)| i64::from(p.qty) * w(i))
            .sum();
        let b: i64 = self
            .pending
            .iter()
            .map(|((_, i), p)| i64::from(p.qty) * w(i))
            .sum();
        a + b
    }

    /// Bir maruziyet grubunun ağırlıklı net pozisyonu (binde), pozisyon + bekleyen.
    pub fn group_net_milli(&self, env: &RiskEnv, group: &str) -> i64 {
        let mut n = 0i64;
        let mut add = |i: &InstrumentId, signed: i64| {
            if let Some(x) = env.instruments.by_id(*i)
                && x.exposure_group.as_deref() == Some(group)
            {
                n += signed * x.exposure_weight_milli;
            }
        };
        for ((_, i), p) in &self.positions {
            add(i, i64::from(p.signed()));
        }
        for ((_, i), p) in &self.pending {
            add(i, i64::from(p.qty.saturating_mul(p.side.sign())));
        }
        n
    }
}
