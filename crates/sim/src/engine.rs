//! Olay tabanlı simülasyon motoru.
//!
//! Her piyasa olayı için sıra (tüm bileşenler aynı saatten beslenir):
//! 1. Sıra kontrolü (geri giden zaman = hata, sessiz düzeltme yok).
//! 2. Süresi dolan giriş emirleri iptal.
//! 3. Önceki olaylarda verilmiş emirler bu olayla eşleşir (`FillModel`); OCO çözümü.
//! 4. Fiyat güncellemesi → `RiskState` (taban, günlük zarar, kill).
//! 5. Zorunlu çıkışlar (kill, `flat_by`, hafta sonu, zaman çıkışı) → bir sonraki olayda piyasa.
//! 6. Olay geçmişe eklenir; stratejiler `Ctx` ile çağrılır; her `Intent` `RiskGate`'ten geçer.
//!
//! Kesinti olayı (`Feed`) fiyat değildir: emir eşleşmez, fiyat güncellenmez (ders §9.2).
//!
//! Ücret tarafı (maker/taker): verildiği anda son fiyata göre karşı tarafa değmeyen (marketable
//! olmayan) ve sonra kitapta bekleyip dolan limit **giriş** emri maker oranını öder
//! (`instruments.toml` `maker_fee_rate_percent`; tanımlı değilse taker). Piyasa, stop, verildiği anda
//! marketable olan limit ve motorun çıkış emirleri (stop, hedef, zorunlu çıkış) taker öder. Hedef
//! (take-profit) limitleri bilinçli olarak taker sayılır (karamsar; araştırma sonuçları değişmesin).

use crate::fill::{BarScope, FillModel, OrderKind, Role, Trigger, WorkingOrder};
use atlas_core::strategy::{AccountView, PositionView, SessionView};
use atlas_core::{
    Action, Ctx, EntryType, Event, EventKind, FeedStatus, Fill, FillReason, Instrument,
    InstrumentId, Intent, Liquidity, MarketEvent, MarketHistory, MarketPayload, Money, Price, Qty,
    Side, Strategy, StrategyId, UnixNanos,
};
use atlas_risk::{
    PendingEntry, RiskEnv, RiskGate, RiskState, Verdict, WEEKEND_FLATTEN_LEAD_MIN, risk_entry_ref,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimConfig {
    /// Enstrüman başına geçmişte tutulacak bar (0 = sınırsız).
    pub max_bars_history: usize,
    /// Kayma çarpanı, binde (1000 = 1×; gauntlet maliyet stresi 2000).
    pub slippage_mult_milli: i64,
    /// Ücret çarpanı, binde.
    pub fee_mult_milli: i64,
    /// Enstrümanın kaymasını geçersiz kıl (tick). `None` = instruments.toml.
    pub slippage_override: Option<i64>,
    /// Emir gecikmesi (ns).
    pub latency_ns: u64,
    /// Saklanacak en fazla red olayı (sayılar her zaman tam tutulur).
    pub max_rejection_events: usize,
    /// `false`: pozisyon açılış/kapanış olay yükleri kurulmaz (yalnız araştırma/gauntlet hızı için;
    /// işlem kayıtları ve risk olayları aynen üretilir).
    pub record_position_events: bool,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            max_bars_history: 0,
            slippage_mult_milli: 1000,
            fee_mult_milli: 1000,
            slippage_override: None,
            latency_ns: 0,
            max_rejection_events: 1000,
            record_position_events: true,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SimError {
    #[error("olay sırası geri gitti: önceki {prev}, gelen {got} (veri kaynağı sıralı olmalı)")]
    OutOfOrder { prev: UnixNanos, got: UnixNanos },
    #[error("bilinmeyen enstrüman {0}")]
    UnknownInstrument(InstrumentId),
    #[error("maliyet eksik: {0}")]
    CostMissing(String),
    #[error("iç tutarsızlık: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeRecord {
    pub strategy: StrategyId,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: i32,
    pub entry_ts: UnixNanos,
    pub entry_price: Price,
    pub exit_ts: UnixNanos,
    pub exit_price: Price,
    pub exit_reason: FillReason,
    /// Dolum fiyatlarıyla brüt (kayma dahil).
    pub gross: Money,
    pub fees: Money,
    /// Kaymanın maliyeti (referans fiyata göre aleyhte fark).
    pub slippage: Money,
    pub net: Money,
}

impl TradeRecord {
    /// Maliyetsiz (referans fiyatlı) sonuç: net + ücret + kayma.
    pub fn frictionless(&self) -> Money {
        self.net + self.fees + self.slippage
    }
    pub fn cost(&self) -> Money {
        self.fees + self.slippage
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimResult {
    pub fill_model: String,
    pub start_balance: Money,
    pub final_balance: Money,
    pub trades: Vec<TradeRecord>,
    pub fills: Vec<Fill>,
    pub events: Vec<Event>,
    pub rejections: BTreeMap<String, u64>,
    pub rejection_events_truncated: u64,
    pub market_events: u64,
}

impl SimResult {
    /// Sonucun bit düzeyinde parmak izi (determinizm testi, kural 2).
    pub fn hash(&self) -> String {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let d = Sha256::digest(&bytes);
        d.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn net_pnl(&self) -> Money {
        self.final_balance - self.start_balance
    }
}

#[derive(Debug, Clone)]
struct EntryMeta {
    stop: Price,
    take_profit: Option<Price>,
    time_exit: Option<UnixNanos>,
}

#[derive(Debug, Clone)]
struct OpenRec {
    side: Side,
    qty: i32,
    entry_ts: UnixNanos,
    entry_price: Price,
    fees: Money,
    slippage: Money,
}

type PosKey = (StrategyId, InstrumentId);

struct AccountAdapter<'a> {
    state: &'a RiskState,
    env: &'a RiskEnv,
}

impl AccountView for AccountAdapter<'_> {
    fn position(&self, strategy: &StrategyId, inst: InstrumentId) -> Option<PositionView> {
        self.state
            .position_of(strategy, inst)
            .map(|p| PositionView {
                side: p.side,
                qty: p.qty,
                entry: p.entry,
                stop: p.stop,
                opened: p.opened,
            })
    }
    fn has_pending_entry(&self, strategy: &StrategyId, inst: InstrumentId) -> bool {
        self.state.has_pending_of(strategy, inst)
    }
    fn equity(&self) -> Money {
        self.state.equity(self.env)
    }
}

pub struct SessionAdapter<'a> {
    pub env: &'a RiskEnv,
}

impl SessionAdapter<'_> {
    fn spec(&self, inst: InstrumentId) -> Option<&atlas_core::SessionSpec> {
        let i = self.env.instruments.by_id(inst)?;
        self.env.calendar.session(&i.session).ok()
    }
}

impl SessionView for SessionAdapter<'_> {
    fn is_open(&self, inst: InstrumentId, ts: UnixNanos) -> bool {
        self.spec(inst).is_some_and(|s| s.is_open(ts))
    }
    fn time_to_session_close(&self, inst: InstrumentId, ts: UnixNanos) -> Option<u64> {
        self.spec(inst)?.next_close(ts).map(|c| c.since(ts))
    }
    fn time_to_week_close(&self, inst: InstrumentId, ts: UnixNanos) -> Option<u64> {
        self.spec(inst)?.next_week_close(ts).map(|c| c.since(ts))
    }
}

/// Motor, risk kapısını ve dolum modelini sahiplenir (`Arc`): uzun yaşayan paper/gölge koşucuları
/// (daemon) ve kısa araştırma koşuları aynı tipi kullanır.
pub struct Engine {
    gate: Arc<RiskGate>,
    model: Arc<dyn FillModel>,
    cfg: SimConfig,
    state: RiskState,
    history: MarketHistory,
    orders: BTreeMap<u64, WorkingOrder>,
    entry_meta: BTreeMap<u64, EntryMeta>,
    /// Verildiği anda marketable olmayan (kitapta bekleyen) limit giriş emirleri: dolunca maker ücreti.
    resting_entries: BTreeSet<u64>,
    open: BTreeMap<PosKey, OpenRec>,
    time_exit: BTreeMap<PosKey, UnixNanos>,
    cost_checked: BTreeMap<InstrumentId, i64>,
    next_id: u64,
    seq: u64,
    last_ts: UnixNanos,
    rejection_events_stored: usize,
    ids: Vec<StrategyId>,
    result: SimResult,
}

impl Engine {
    pub fn new(
        gate: Arc<RiskGate>,
        model: Arc<dyn FillModel>,
        cfg: SimConfig,
        start: UnixNanos,
    ) -> Self {
        let mut state = RiskState::new(&gate.env, start);
        state.quiet = !cfg.record_position_events;
        let start_balance = state.balance;
        let fill_model = format!("{}@v{}", model.name(), model.version());
        Engine {
            gate,
            model,
            history: MarketHistory::new(cfg.max_bars_history),
            cfg,
            state,
            orders: BTreeMap::new(),
            entry_meta: BTreeMap::new(),
            resting_entries: BTreeSet::new(),
            open: BTreeMap::new(),
            time_exit: BTreeMap::new(),
            cost_checked: BTreeMap::new(),
            next_id: 1,
            seq: 0,
            last_ts: start,
            rejection_events_stored: 0,
            ids: Vec::new(),
            result: SimResult {
                fill_model,
                start_balance,
                final_balance: start_balance,
                trades: Vec::new(),
                fills: Vec::new(),
                events: Vec::new(),
                rejections: BTreeMap::new(),
                rejection_events_truncated: 0,
                market_events: 0,
            },
        }
    }

    pub fn state(&self) -> &RiskState {
        &self.state
    }

    /// Risk ortamı (prop kuralları, politika, enstrümanlar) — salt okunur.
    pub fn gate_env(&self) -> &atlas_risk::RiskEnv {
        &self.gate.env
    }

    /// Haber zamanları vb. dış durumu ayarlamak için.
    /// Biriken olayları alır (canlı koşucu her adımda boşaltır).
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.result.events)
    }

    /// Şu ana kadar kapanmış işlemler.
    pub fn trades(&self) -> &[TradeRecord] {
        &self.result.trades
    }

    pub fn last_ts(&self) -> UnixNanos {
        self.last_ts
    }

    pub fn state_mut(&mut self) -> &mut RiskState {
        &mut self.state
    }

    /// Ortamın paylaşılan kopyası (alan ödünçlerinden bağımsız).
    fn gate(&self) -> Arc<RiskGate> {
        Arc::clone(&self.gate)
    }

    fn instrument(gate: &RiskGate, id: InstrumentId) -> Result<&Instrument, SimError> {
        gate.env
            .instruments
            .by_id(id)
            .ok_or(SimError::UnknownInstrument(id))
    }

    /// Enstrümanın kaymasını (çarpan dahil) döndürür; maliyet alanları eksikse hata.
    fn slippage_for(&mut self, id: InstrumentId) -> Result<i64, SimError> {
        if let Some(s) = self.cost_checked.get(&id) {
            return Ok(*s);
        }
        let gate = self.gate();
        let inst = Self::instrument(&gate, id)?;
        let base = match self.cfg.slippage_override {
            Some(s) => s,
            None => inst
                .slippage_ticks()
                .map_err(|e| SimError::CostMissing(e.to_string()))?,
        };
        inst.fee(Price(1), Qty(1))
            .map_err(|e| SimError::CostMissing(e.to_string()))?;
        let s = base.saturating_mul(self.cfg.slippage_mult_milli) / 1000;
        self.cost_checked.insert(id, s);
        Ok(s)
    }

    fn push_events(&mut self, evs: Vec<Event>) {
        self.result.events.extend(evs);
    }

    pub fn run(
        mut self,
        events: impl IntoIterator<Item = MarketEvent>,
        strategies: &mut [Box<dyn Strategy + '_>],
    ) -> Result<SimResult, SimError> {
        for ev in events {
            self.step(&ev, strategies)?;
        }
        self.finish()
    }

    pub fn step(
        &mut self,
        ev: &MarketEvent,
        strategies: &mut [Box<dyn Strategy + '_>],
    ) -> Result<(), SimError> {
        if ev.ts < self.last_ts {
            return Err(SimError::OutOfOrder {
                prev: self.last_ts,
                got: ev.ts,
            });
        }
        self.seq += 1;
        self.last_ts = ev.ts;
        self.result.market_events += 1;
        let slip = self.slippage_for(ev.instrument)?;

        if let MarketPayload::Feed(status) = &ev.payload {
            let (kind, payload) = match status {
                FeedStatus::Down { reason } => (
                    EventKind::FeedDown,
                    serde_json::json!({"instrument": ev.instrument.0, "reason": reason}),
                ),
                FeedStatus::Up => (
                    EventKind::FeedUp,
                    serde_json::json!({"instrument": ev.instrument.0}),
                ),
            };
            self.push_events(vec![Event::new(ev.ts, kind, payload)]);
            self.history.push(ev);
            return self.run_strategies(ev, strategies);
        }

        // Durumlu dolum modelleri (kuyruk) her olayı görür (kotasyon/işlem akışı).
        self.model.observe(ev);
        self.expire_entries(ev.ts);
        if self.model.handles(&ev.payload) {
            self.match_event(ev, slip)?;
        }
        if let Some(px) = ev.last_price() {
            let evs = self
                .state
                .on_price(&self.gate.env, ev.ts, ev.instrument, px);
            self.push_events(evs);
        }
        self.forced_exits(ev.ts);
        self.history.push(ev);
        self.run_strategies(ev, strategies)
    }

    fn expire_entries(&mut self, ts: UnixNanos) {
        let expired: Vec<u64> = self
            .orders
            .values()
            .filter(|o| o.role == Role::Entry && o.expiry.is_some_and(|e| e <= ts))
            .map(|o| o.id)
            .collect();
        for id in expired {
            if let Some(o) = self.orders.remove(&id) {
                self.model.order_closed(id);
                self.entry_meta.remove(&id);
                self.resting_entries.remove(&id);
                self.state.cancel_pending(&o.strategy, o.instrument);
            }
        }
    }

    fn match_event(&mut self, ev: &MarketEvent, slip: i64) -> Result<(), SimError> {
        let seq = self.seq;
        let triggers: Vec<Trigger> = self
            .orders
            .values()
            .filter(|o| {
                o.instrument == ev.instrument && o.created_seq < seq && o.active_from <= ev.ts
            })
            .filter_map(|o| self.model.match_one(ev, o, slip, BarScope::Full))
            .collect();
        if triggers.is_empty() {
            return Ok(());
        }
        for t in self.resolve_oco(triggers) {
            self.execute(ev, t, slip)?;
        }
        Ok(())
    }

    /// Aynı pozisyonun çıkış emirlerinden yalnız biri dolar: önce açılışta tetiklenen,
    /// sonra stop (aynı barda stop+hedef → stop), sonra motor çıkışı, sonra hedef.
    fn resolve_oco(&self, triggers: Vec<Trigger>) -> Vec<Trigger> {
        let rank = |r: Role| match r {
            Role::StopLoss => 0,
            Role::Exit(_) => 1,
            Role::TakeProfit => 2,
            Role::Entry => 3,
        };
        let mut best: BTreeMap<(PosKey, bool), (Trigger, Role)> = BTreeMap::new();
        for t in triggers {
            let Some(o) = self.orders.get(&t.order_id) else {
                continue;
            };
            let key = ((o.strategy.clone(), o.instrument), o.role.is_exit());
            let better = match best.get(&key) {
                None => true,
                Some((b, br)) => {
                    (t.at_open && !b.at_open)
                        || (t.at_open == b.at_open && rank(o.role) < rank(*br))
                }
            };
            if better {
                best.insert(key, (t, o.role));
            }
        }
        let mut v: Vec<Trigger> = best.into_values().map(|(t, _)| t).collect();
        v.sort_by_key(|t| t.order_id);
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn new_order(
        &mut self,
        strategy: StrategyId,
        instrument: InstrumentId,
        side: Side,
        qty: i32,
        kind: OrderKind,
        role: Role,
        ts: UnixNanos,
        expiry: Option<UnixNanos>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.orders.insert(
            id,
            WorkingOrder {
                id,
                strategy,
                instrument,
                side,
                qty,
                kind,
                role,
                active_from: ts.plus(self.cfg.latency_ns),
                created_seq: self.seq,
                expiry,
            },
        );
        id
    }

    fn execute(&mut self, ev: &MarketEvent, t: Trigger, slip: i64) -> Result<(), SimError> {
        let Some(o) = self.orders.remove(&t.order_id) else {
            return Ok(());
        };
        let gate = self.gate();
        let inst = Self::instrument(&gate, o.instrument)?;
        // Kitapta bekleyip dolan limit girişi maker; diğer her dolum taker (modül belgesi).
        let liquidity = if self.resting_entries.remove(&o.id) {
            Liquidity::Maker
        } else {
            Liquidity::Taker
        };
        let fee = inst
            .fee_for(t.price, Qty(o.qty), liquidity)
            .map_err(|e| SimError::CostMissing(e.to_string()))?;
        let fee = Money(fee.0.saturating_mul(self.cfg.fee_mult_milli) / 1000);
        let slippage_cost = inst.tick_value * ((t.price - t.reference).abs() * i64::from(o.qty));
        let fill = Fill {
            ts: ev.ts,
            strategy: o.strategy.clone(),
            instrument: o.instrument,
            side: o.side,
            qty: o.qty,
            price: t.price,
            fee,
            reason: o.role.fill_reason(),
        };
        let evs = self.state.on_fill(&self.gate.env, &fill);
        self.push_events(evs);
        if self.cfg.record_position_events {
            self.result.fills.push(fill);
        }
        let key = (o.strategy.clone(), o.instrument);

        if o.role == Role::Entry {
            let meta = self
                .entry_meta
                .remove(&o.id)
                .ok_or_else(|| SimError::Internal(format!("giriş emri {} için meta yok", o.id)))?;
            self.open.insert(
                key.clone(),
                OpenRec {
                    side: o.side,
                    qty: o.qty,
                    entry_ts: ev.ts,
                    entry_price: t.price,
                    fees: fee,
                    slippage: slippage_cost,
                },
            );
            if let Some(te) = meta.time_exit {
                self.time_exit.insert(key.clone(), te);
            }
            let exit_side = o.side.opposite();
            let mut brackets = vec![self.new_order(
                o.strategy.clone(),
                o.instrument,
                exit_side,
                o.qty,
                OrderKind::Stop(meta.stop),
                Role::StopLoss,
                ev.ts,
                None,
            )];
            if let Some(tp) = meta.take_profit {
                brackets.push(self.new_order(
                    o.strategy.clone(),
                    o.instrument,
                    exit_side,
                    o.qty,
                    OrderKind::Limit(tp),
                    Role::TakeProfit,
                    ev.ts,
                    None,
                ));
            }
            // Bar modelinde girişle aynı barın geri kalanı da kontrol edilir.
            if matches!(ev.payload, MarketPayload::Bar(_)) {
                let scope = if t.at_open {
                    BarScope::Full
                } else {
                    BarScope::AfterIntrabarEntry
                };
                let trig = {
                    let refs: Vec<&WorkingOrder> = brackets
                        .iter()
                        .filter_map(|id| self.orders.get(id))
                        .collect();
                    self.model.match_orders(ev, &refs, slip, scope)
                };
                for t2 in self.resolve_oco(trig) {
                    self.execute(ev, t2, slip)?;
                }
            }
        } else {
            // OCO: kardeş çıkış emirlerini iptal et.
            let siblings: Vec<u64> = self
                .orders
                .values()
                .filter(|x| {
                    x.role.is_exit() && x.strategy == o.strategy && x.instrument == o.instrument
                })
                .map(|x| x.id)
                .collect();
            for id in siblings {
                self.orders.remove(&id);
                self.model.order_closed(id);
            }
            self.time_exit.remove(&key);
            let rec = self.open.remove(&key).ok_or_else(|| {
                SimError::Internal(format!("çıkış dolumu ama açık işlem kaydı yok: {key:?}"))
            })?;
            let signed = Qty(rec.qty.saturating_mul(rec.side.sign()));
            let gross = inst.pnl(rec.entry_price, t.price, signed);
            let fees = rec.fees + fee;
            self.result.trades.push(TradeRecord {
                strategy: o.strategy,
                instrument: o.instrument,
                side: rec.side,
                qty: rec.qty,
                entry_ts: rec.entry_ts,
                entry_price: rec.entry_price,
                exit_ts: ev.ts,
                exit_price: t.price,
                exit_reason: o.role.fill_reason(),
                gross,
                fees,
                slippage: rec.slippage + slippage_cost,
                net: gross - fees,
            });
        }
        Ok(())
    }

    fn has_exit_order(&self, key: &PosKey) -> bool {
        self.orders.values().any(|o| {
            matches!(o.role, Role::Exit(_)) && o.strategy == key.0 && o.instrument == key.1
        })
    }

    fn submit_exit(&mut self, key: &PosKey, reason: FillReason, ts: UnixNanos) {
        if self.has_exit_order(key) {
            return;
        }
        let Some(p) = self.state.positions().get(key).cloned() else {
            return;
        };
        self.new_order(
            key.0.clone(),
            key.1,
            p.side.opposite(),
            p.qty,
            OrderKind::Market,
            Role::Exit(reason),
            ts,
            None,
        );
    }

    fn cancel_entry(&mut self, key: &PosKey) {
        let ids: Vec<u64> = self
            .orders
            .values()
            .filter(|o| o.role == Role::Entry && o.strategy == key.0 && o.instrument == key.1)
            .map(|o| o.id)
            .collect();
        for id in ids {
            self.orders.remove(&id);
            self.model.order_closed(id);
            self.entry_meta.remove(&id);
            self.resting_entries.remove(&id);
        }
        self.state.cancel_pending(&key.0, key.1);
    }

    fn forced_exits(&mut self, ts: UnixNanos) {
        let gate = self.gate();
        let env = &gate.env;
        let must_flat = env.must_be_flat(ts);
        let mut exits: Vec<(PosKey, FillReason)> = Vec::new();
        for key in self.state.positions().keys() {
            let reason = if self.state.kills.blocking(&key.0).is_some() {
                Some(FillReason::KillSwitch)
            } else if must_flat || env.near_week_close(key.1, ts, WEEKEND_FLATTEN_LEAD_MIN) {
                Some(FillReason::SessionEnd)
            } else if self.time_exit.get(key).is_some_and(|t| *t <= ts) {
                Some(FillReason::TimeExit)
            } else {
                None
            };
            if let Some(r) = reason {
                exits.push((key.clone(), r));
            }
        }
        for (k, r) in exits {
            self.submit_exit(&k, r, ts);
        }
        let killed_pending: Vec<PosKey> = self
            .state
            .pending()
            .keys()
            .filter(|k| self.state.kills.blocking(&k.0).is_some() || must_flat)
            .cloned()
            .collect();
        for k in killed_pending {
            self.cancel_entry(&k);
        }
    }

    fn run_strategies(
        &mut self,
        ev: &MarketEvent,
        strategies: &mut [Box<dyn Strategy + '_>],
    ) -> Result<(), SimError> {
        let mut intents: Vec<Intent> = Vec::new();
        {
            let gate = self.gate();
            let env = &gate.env;
            let acct = AccountAdapter {
                state: &self.state,
                env,
            };
            let sess = SessionAdapter { env };
            if self.ids.len() != strategies.len() {
                self.ids = strategies.iter().map(|s| s.id().clone()).collect();
            }
            for (s, id) in strategies.iter_mut().zip(self.ids.iter()) {
                let ctx = Ctx::new(id, &self.history, &acct, &sess);
                for mut i in s.on_event(&ctx, ev) {
                    // Strateji başkası adına ya da başka zamanda niyet üretemez.
                    i.strategy = id.clone();
                    i.ts = ev.ts;
                    intents.push(i);
                }
            }
        }
        for i in intents {
            self.handle_intent(i)?;
        }
        Ok(())
    }

    fn handle_intent(&mut self, intent: Intent) -> Result<(), SimError> {
        let d = self.gate.check(&self.state, &intent);
        self.push_events(d.events);
        let key = (intent.strategy.clone(), intent.instrument);
        match d.verdict {
            Verdict::Reject(r) => {
                let code = serde_json::to_value(&r)
                    .ok()
                    .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
                    .unwrap_or_else(|| "unknown".into());
                *self.result.rejections.entry(code).or_default() += 1;
                if self.rejection_events_stored < self.cfg.max_rejection_events {
                    self.rejection_events_stored += 1;
                    self.result.events.push(Event::new(
                        intent.ts,
                        EventKind::IntentRejected,
                        serde_json::json!({"strategy": intent.strategy, "instrument": intent.instrument.0, "reason": r}),
                    ));
                } else {
                    self.result.rejection_events_truncated += 1;
                }
            }
            Verdict::Approve => match intent.action {
                Action::Open {
                    side,
                    qty,
                    entry,
                    stop_loss,
                    take_profit,
                    time_exit,
                    entry_expiry,
                } => {
                    let kind = match entry {
                        EntryType::Market => OrderKind::Market,
                        EntryType::Limit(p) => OrderKind::Limit(p),
                        EntryType::Stop(p) => OrderKind::Stop(p),
                    };
                    // Bekleyen girişin riski, kapının kullandığı en kötü dolum fiyatıyla izlenir.
                    let entry_ref =
                        risk_entry_ref(side, entry, self.state.last_price(intent.instrument))
                            .ok_or_else(|| {
                                SimError::Internal("onaylı giriş ama referans fiyat yok".into())
                            })?;
                    self.state.register_pending(
                        intent.strategy.clone(),
                        intent.instrument,
                        PendingEntry {
                            side,
                            qty,
                            entry_ref,
                            stop: stop_loss,
                        },
                    );
                    // Verildiği anda marketable mı? Son fiyat bilinmiyorsa karamsar: taker.
                    let resting = match (entry, self.state.last_price(intent.instrument)) {
                        (EntryType::Limit(p), Some(last)) => match side {
                            Side::Buy => p < last,
                            Side::Sell => p > last,
                        },
                        _ => false,
                    };
                    let id = self.new_order(
                        intent.strategy,
                        intent.instrument,
                        side,
                        qty,
                        kind,
                        Role::Entry,
                        intent.ts,
                        entry_expiry,
                    );
                    if resting {
                        self.resting_entries.insert(id);
                    }
                    self.entry_meta.insert(
                        id,
                        EntryMeta {
                            stop: stop_loss,
                            take_profit,
                            time_exit,
                        },
                    );
                }
                Action::Close { .. } => {
                    if self.state.positions().contains_key(&key) {
                        self.submit_exit(&key, FillReason::Close, intent.ts);
                    } else {
                        self.cancel_entry(&key);
                    }
                }
                Action::CancelEntry => self.cancel_entry(&key),
            },
        }
        Ok(())
    }

    /// Veri bitti: açık pozisyonlar son fiyattan kaymasız kapanır, emirler iptal.
    pub fn finish(mut self) -> Result<SimResult, SimError> {
        let keys: Vec<PosKey> = self.state.positions().keys().cloned().collect();
        let ts = self.last_ts;
        for key in keys {
            let Some(px) = self.state.last_price(key.1) else {
                continue;
            };
            let Some(p) = self.state.positions().get(&key).cloned() else {
                continue;
            };
            // Bu pozisyon için mevcut çıkış emirlerini kaldır; kapanışı doğrudan yaz.
            self.orders
                .retain(|_, o| !(o.strategy == key.0 && o.instrument == key.1));
            let id = self.new_order(
                key.0.clone(),
                key.1,
                p.side.opposite(),
                p.qty,
                OrderKind::Market,
                Role::Exit(FillReason::EndOfData),
                ts,
                None,
            );
            let ev = MarketEvent::trade(
                key.1,
                ts,
                atlas_core::Trade {
                    price: px,
                    size: 0,
                    aggressor: None,
                },
            );
            self.execute(
                &ev,
                Trigger {
                    order_id: id,
                    price: px,
                    reference: px,
                    at_open: false,
                },
                0,
            )?;
        }
        self.orders.clear();
        self.result.final_balance = self.state.balance;
        Ok(self.result)
    }
}

#[cfg(test)]
mod maker_taker_tests {
    //! Maker/taker ücret ayrımı: kitapta bekleyen limit girişi maker, geri kalan her dolum taker.
    use super::*;
    use crate::fill::BarFill;
    use crate::validation::{RandomEntry, VALIDATION_CALENDAR, VALIDATION_POLICY, VALIDATION_PROP};
    use atlas_core::units::NANOS_PER_MIN;
    use atlas_core::{Bar, Calendar, InstrumentTable};
    use atlas_risk::{PropRules, RiskPolicy};

    /// BTCUSDT benzeri: tick 0.1, lot 0.001, taker %0,05; `maker` verilirse maker oranı.
    fn env(maker: Option<&str>) -> RiskEnv {
        let maker_line = maker
            .map(|m| format!("maker_fee_rate_percent = \"{m}\"\n"))
            .unwrap_or_default();
        let inst = format!(
            "[[instrument]]\nsymbol = \"BTCUSDT\"\nvenue = \"binance\"\nkind = \"perpetual\"\n\
             tick_size = \"0.1\"\ntick_value_usd = \"0.0001\"\nlot_size = \"0.001\"\nsession = \"always\"\n\
             [instrument.costs]\nfee_rate_percent = \"0.05\"\n{maker_line}slippage_ticks = \"1\"\n"
        );
        RiskEnv {
            rules: PropRules::from_toml_str(VALIDATION_PROP).unwrap(),
            policy: RiskPolicy::from_toml_str(VALIDATION_POLICY).unwrap(),
            instruments: InstrumentTable::from_toml_str(&inst).unwrap(),
            calendar: Calendar::from_toml_str(VALIDATION_CALENDAR).unwrap(),
        }
    }

    /// Belirli bar sırasında belirli niyeti üreten betik strateji.
    struct Script {
        id: StrategyId,
        n: usize,
        plan: Vec<(usize, Action)>,
    }

    impl Strategy for Script {
        fn id(&self) -> &StrategyId {
            &self.id
        }
        fn on_event(&mut self, _ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
            let i = self.n;
            self.n += 1;
            self.plan
                .iter()
                .filter(|(k, _)| *k == i)
                .map(|(_, a)| Intent {
                    strategy: self.id.clone(),
                    instrument: InstrumentId(0),
                    ts: ev.ts,
                    action: a.clone(),
                })
                .collect()
        }
    }

    /// Fiyatlar tick cinsinden (78050.0 = 780_500).
    fn bar(i: u64, o: i64, h: i64, l: i64, c: i64) -> MarketEvent {
        MarketEvent::bar(
            InstrumentId(0),
            Bar {
                open_ts: UnixNanos(i * NANOS_PER_MIN),
                close_ts: UnixNanos((i + 1) * NANOS_PER_MIN),
                open: Price(o),
                high: Price(h),
                low: Price(l),
                close: Price(c),
                volume: 10,
                buy_volume: 5,
                trades: 1,
            },
        )
    }

    fn open(side: Side, qty: i32, entry: EntryType, stop: i64) -> Action {
        Action::Open {
            side,
            qty,
            entry,
            stop_loss: Price(stop),
            take_profit: None,
            time_exit: None,
            entry_expiry: None,
        }
    }

    fn run(env: RiskEnv, plan: Vec<(usize, Action)>, bars: &[MarketEvent]) -> SimResult {
        let engine = Engine::new(
            Arc::new(RiskGate::new(env)),
            Arc::new(BarFill::default()),
            SimConfig::default(),
            UnixNanos(0),
        );
        let mut s: Vec<Box<dyn Strategy>> = vec![Box::new(Script {
            id: StrategyId::new("s"),
            n: 0,
            plan,
        })];
        engine.run(bars.iter().cloned(), &mut s).unwrap()
    }

    fn fill(r: &SimResult, reason: FillReason) -> Fill {
        r.fills
            .iter()
            .find(|f| f.reason == reason)
            .cloned()
            .unwrap_or_else(|| panic!("{reason:?} dolumu yok: {:?}", r.fills))
    }

    /// Bar 0 kapanışı 78100.0; bar 2 78050.0'ın içinden geçer; bar 3 stopu (77900.0) vurur.
    fn bars() -> Vec<MarketEvent> {
        vec![
            bar(0, 781_000, 781_050, 780_950, 781_000),
            bar(1, 781_000, 781_020, 780_700, 780_800),
            bar(2, 780_800, 780_900, 780_400, 780_600),
            bar(3, 780_600, 780_600, 778_000, 778_500),
        ]
    }

    /// Grok'un 2 BTC işlemi: 78050.0 alış limiti, son fiyat 78100.0 (verildiği anda marketable değil).
    fn grok_plan() -> Vec<(usize, Action)> {
        vec![(
            0,
            open(Side::Buy, 2000, EntryType::Limit(Price(780_500)), 779_000),
        )]
    }

    #[test]
    fn bekleyen_limit_giris_maker_oder() {
        let r = run(env(Some("0.02")), grok_plan(), &bars());
        let f = fill(&r, FillReason::Entry);
        assert_eq!(f.price, Price(780_500), "limit fiyatından, kaymasız");
        // 2 BTC × 78050 = $156.100 notional; maker %0,02 → $31,22
        assert_eq!(f.fee, Money(31_220_000));
        // Maker alanı boş → eski davranış: taker %0,05 → $78,05 (VPS journal'daki ücretle aynı).
        let old = run(env(None), grok_plan(), &bars());
        assert_eq!(fill(&old, FillReason::Entry).fee, Money(78_050_000));
    }

    #[test]
    fn marketable_limit_ve_piyasa_taker_oder() {
        // Alış limiti 78110.0 ≥ son fiyat 78100.0 → verildiği anda marketable → taker.
        let marketable = vec![(
            0,
            open(Side::Buy, 2000, EntryType::Limit(Price(781_100)), 779_000),
        )];
        let r = run(env(Some("0.02")), marketable, &bars());
        let f = fill(&r, FillReason::Entry);
        assert_eq!(f.price, Price(781_000), "açılıştan (limitten iyi) dolar");
        assert_eq!(f.fee, Money(78_100_000), "taker %0,05");
        // Piyasa emri: açılış + 1 tick kayma, taker.
        let market = vec![(0, open(Side::Buy, 2000, EntryType::Market, 779_000))];
        let r = run(env(Some("0.02")), market, &bars());
        let f = fill(&r, FillReason::Entry);
        assert_eq!(f.price, Price(781_001));
        assert_eq!(f.fee, Money(78_100_100), "2 × 78100.1 × %0,05");
    }

    #[test]
    fn stop_cikisi_taker_oder() {
        let r = run(env(Some("0.02")), grok_plan(), &bars());
        let stop = fill(&r, FillReason::StopLoss);
        // Stop 77900.0, satış yönünde 1 tick kayma → 77899.9; taker %0,05 → $77,8999 (journal'daki gibi)
        assert_eq!(stop.price, Price(778_999));
        assert_eq!(stop.fee, Money(77_899_900));
        assert_eq!(r.trades.len(), 1);
        assert_eq!(r.trades[0].fees, Money(31_220_000 + 77_899_900));
    }

    #[test]
    fn stop_giris_taker_oder() {
        let plan = vec![(
            0,
            open(Side::Sell, 1000, EntryType::Stop(Price(780_700)), 782_000),
        )];
        let e = env(Some("0.02"));
        let inst = e.instruments.get("BTCUSDT").unwrap().clone();
        let r = run(e, plan, &bars());
        let f = fill(&r, FillReason::Entry);
        assert_eq!(f.fee, inst.fee(f.price, Qty(1000)).unwrap());
    }

    /// Piyasa girişli + hedef limitli stratejiler (genomlar böyle) maker oranından etkilenmez.
    #[test]
    fn piyasa_girisli_stratejiler_degismez() {
        let bars = crate::synthetic::ar_bars(11, 3000, UnixNanos(0), 0.1, 40.0, Price(700_000));
        let evs: Vec<MarketEvent> = bars
            .iter()
            .map(|b| MarketEvent::bar(InstrumentId(0), *b))
            .collect();
        let go = |maker: Option<&str>| {
            let engine = Engine::new(
                Arc::new(RiskGate::new(env(maker))),
                Arc::new(BarFill::default()),
                SimConfig::default(),
                UnixNanos(0),
            );
            let mut s: Vec<Box<dyn Strategy>> = vec![Box::new(RandomEntry::new(
                7,
                InstrumentId(0),
                0.05,
                300,
                600,
                30 * NANOS_PER_MIN,
            ))];
            engine.run(evs.iter().cloned(), &mut s).unwrap()
        };
        let a = go(None);
        let b = go(Some("0.02"));
        assert!(a.trades.len() > 20, "{}", a.trades.len());
        assert_eq!(
            a.hash(),
            b.hash(),
            "maker oranı piyasa girişlerini değiştirmemeli"
        );
    }
}
