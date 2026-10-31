//! M1 kabul doğrulamaları (§4.4, §7 M1):
//! - Rastgele giriş stratejisinin beklenen sonucu ≈ −maliyet (istatistiksel test).
//! - Al-tut fiyat farkını birebir verir.
//! - Aynı mumda stop+hedef → stop.
//! - Gap'te stop dolumu açılış fiyatından.
//! - Aynı seed → bit düzeyinde aynı sonuç.
//!
//! Buradaki hesap/enstrüman kuralları **doğrulama amaçlı kurgusaldır**, gerçek bir firmayı temsil etmez.

use crate::engine::{Engine, SimConfig, SimError, SimResult};
use crate::fill::{BarFill, FillModel, LimitRule, TradeFill};
use crate::synthetic::{bars_from_trades, random_walk_trades};
use atlas_core::calendar::Calendar;
use atlas_core::units::{NANOS_PER_MIN, NANOS_PER_SEC};
use atlas_core::{
    Action, Bar, Ctx, EntryType, EventKind, FillReason, InstrumentId, InstrumentTable, Intent,
    MarketEvent, Money, Price, Side, Strategy, StrategyId, UnixNanos,
};
use atlas_risk::{DrawdownType, PendingEntry, PropRules, RiskEnv, RiskGate, RiskPolicy, RiskState};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;
use statrs::distribution::{ContinuousCDF, StudentsT};

pub const VALIDATION_PROP: &str = r#"
firm = "DOĞRULAMA (kurgusal, gerçek firma değil)"
account_label = "validation"
starting_balance_usd = "1000000000"
profit_target_usd = "1"
drawdown_type = "static"
drawdown_amount_usd = "999999999"
drawdown_lock_level_usd = "none"
daily_loss_limit_usd = "none"
daily_loss_reference = "day_start_balance"
day_reset_tz = "UTC"
day_reset_time = "00:00"
consistency_ratio = "none"
max_contracts = "1000000"
rules_tz = "UTC"
no_new_entries_after = "none"
flat_by = "none"
weekend_holding_allowed = true
news_blackout_minutes_before = "none"
news_blackout_minutes_after = "none"
algo_trading_allowed = true
"#;

pub const VALIDATION_POLICY: &str = r#"
[risk]
cooldown_after_stop_minutes = 0
max_risk_per_trade_usd = "1000000000"
max_group_net_contracts = "1000000"
consistency_headroom = "1"
drawdown_near = "0"
stop_slippage_buffer_ticks = 0
min_minutes_before_week_close = 0
"#;

/// Kurgusal sentetik enstrüman: tick = $1, kontrat başı $1 komisyon, 1 tick kayma.
pub const VALIDATION_INSTRUMENTS: &str = r#"
[[instrument]]
symbol = "SYN"
venue = "binance"
kind = "future"
tick_size = "1"
tick_value_usd = "1"
session = "always"
[instrument.costs]
commission_per_contract_usd = "1"
slippage_ticks = "1"
"#;

pub const VALIDATION_CALENDAR: &str = r#"
[[session]]
name = "always"
always = true
"#;

pub fn validation_env() -> Result<RiskEnv, String> {
    Ok(RiskEnv {
        rules: PropRules::from_toml_str(VALIDATION_PROP).map_err(|e| e.to_string())?,
        policy: RiskPolicy::from_toml_str(VALIDATION_POLICY).map_err(|e| e.to_string())?,
        instruments: InstrumentTable::from_toml_str(VALIDATION_INSTRUMENTS)
            .map_err(|e| e.to_string())?,
        calendar: Calendar::from_toml_str(VALIDATION_CALENDAR).map_err(|e| e.to_string())?,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckOutcome {
    pub name: String,
    pub pass: bool,
    pub detail: serde_json::Value,
}

impl CheckOutcome {
    fn new(name: &str, pass: bool, detail: serde_json::Value) -> Self {
        CheckOutcome {
            name: name.to_string(),
            pass,
            detail,
        }
    }
    fn error(name: &str, e: impl std::fmt::Display) -> Self {
        Self::new(name, false, serde_json::json!({"error": e.to_string()}))
    }
}

/// Rastgele yönde, rastgele anda giren strateji. Simetrik stop/hedef + zaman çıkışı.
pub struct RandomEntry {
    id: StrategyId,
    inst: InstrumentId,
    rng: ChaCha8Rng,
    p_enter: f64,
    stop_ticks: i64,
    target_ticks: i64,
    hold_ns: u64,
}

impl RandomEntry {
    pub fn new(
        seed: u64,
        inst: InstrumentId,
        p_enter: f64,
        stop_ticks: i64,
        target_ticks: i64,
        hold_ns: u64,
    ) -> Self {
        RandomEntry {
            id: StrategyId::new(format!("random-{seed}")),
            inst,
            rng: ChaCha8Rng::seed_from_u64(seed ^ 0x05ee_d0fa_71a5),
            p_enter,
            stop_ticks,
            target_ticks,
            hold_ns,
        }
    }
}

impl Strategy for RandomEntry {
    fn id(&self) -> &StrategyId {
        &self.id
    }
    fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
        if ev.instrument != self.inst || !ctx.is_flat(self.inst) {
            return Vec::new();
        }
        let Some(last) = ctx.last_price(self.inst) else {
            return Vec::new();
        };
        if !self.rng.random_bool(self.p_enter) {
            return Vec::new();
        }
        let side = if self.rng.random_bool(0.5) {
            Side::Buy
        } else {
            Side::Sell
        };
        let sg = i64::from(side.sign());
        vec![Intent {
            strategy: self.id.clone(),
            instrument: self.inst,
            ts: ctx.now(),
            action: Action::Open {
                side,
                qty: 1,
                entry: EntryType::Market,
                stop_loss: last - sg * self.stop_ticks,
                take_profit: Some(last + sg * self.target_ticks),
                time_exit: Some(ctx.now().plus(self.hold_ns)),
                entry_expiry: None,
            },
        }]
    }
}

/// İlk olayda bir kontrat alır, veri sonuna kadar tutar.
pub struct BuyHold {
    id: StrategyId,
    inst: InstrumentId,
    done: bool,
}

impl BuyHold {
    pub fn new(inst: InstrumentId) -> Self {
        BuyHold {
            id: StrategyId::new("buy-hold"),
            inst,
            done: false,
        }
    }
}

impl Strategy for BuyHold {
    fn id(&self) -> &StrategyId {
        &self.id
    }
    fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
        if self.done || ev.instrument != self.inst {
            return Vec::new();
        }
        self.done = true;
        vec![Intent {
            strategy: self.id.clone(),
            instrument: self.inst,
            ts: ctx.now(),
            action: Action::Open {
                side: Side::Buy,
                qty: 1,
                entry: EntryType::Market,
                stop_loss: Price(0),
                take_profit: None,
                time_exit: None,
                entry_expiry: None,
            },
        }]
    }
}

/// İlk olayda sabit stop/hedefle tek giriş (senaryo testleri).
struct OneShot {
    id: StrategyId,
    inst: InstrumentId,
    side: Side,
    stop: Price,
    target: Price,
    done: bool,
}

impl Strategy for OneShot {
    fn id(&self) -> &StrategyId {
        &self.id
    }
    fn on_event(&mut self, ctx: &Ctx<'_>, _ev: &MarketEvent) -> Vec<Intent> {
        if self.done {
            return Vec::new();
        }
        self.done = true;
        vec![Intent {
            strategy: self.id.clone(),
            instrument: self.inst,
            ts: ctx.now(),
            action: Action::Open {
                side: self.side,
                qty: 1,
                entry: EntryType::Market,
                stop_loss: self.stop,
                take_profit: Some(self.target),
                time_exit: None,
                entry_expiry: None,
            },
        }]
    }
}

pub fn run_sim(
    env: &RiskEnv,
    model: &dyn FillModel,
    cfg: SimConfig,
    events: Vec<MarketEvent>,
    strategies: &mut [Box<dyn Strategy + '_>],
) -> Result<SimResult, SimError> {
    let gate = RiskGate::new(env.clone());
    let start = events.first().map_or(UnixNanos::ZERO, |e| e.ts);
    let engine = Engine::new(&gate, model, cfg, start);
    engine.run(events, strategies)
}

#[derive(Debug, Clone, Serialize)]
pub struct RandomStats {
    pub trades: usize,
    pub mean_net_usd: f64,
    pub mean_cost_usd: f64,
    pub mean_frictionless_usd: f64,
    pub sd_frictionless_usd: f64,
    pub t_stat: f64,
    pub p_value: f64,
}

/// Maliyetsiz sonuçların ortalamasının 0'dan farkı için iki yönlü t-testi.
pub fn random_stats(res: &SimResult) -> RandomStats {
    let xs: Vec<f64> = res
        .trades
        .iter()
        .map(|t| t.frictionless().as_usd_f64())
        .collect();
    let n = xs.len();
    let mean = |v: &[f64]| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let m = mean(&xs);
    let var = if n > 1 {
        xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n as f64 - 1.0)
    } else {
        0.0
    };
    let sd = var.sqrt();
    let t = if sd > 0.0 {
        m / (sd / (n as f64).sqrt())
    } else {
        0.0
    };
    let p = match StudentsT::new(0.0, 1.0, (n.max(2) - 1) as f64) {
        Ok(d) => 2.0 * (1.0 - d.cdf(t.abs())),
        Err(_) => 0.0,
    };
    let nets: Vec<f64> = res.trades.iter().map(|t| t.net.as_usd_f64()).collect();
    let costs: Vec<f64> = res.trades.iter().map(|t| t.cost().as_usd_f64()).collect();
    RandomStats {
        trades: n,
        mean_net_usd: mean(&nets),
        mean_cost_usd: mean(&costs),
        mean_frictionless_usd: m,
        sd_frictionless_usd: sd,
        t_stat: t,
        p_value: p,
    }
}

/// p bu eşiğin altındaysa model sapmalı sayılır.
pub const RANDOM_TEST_ALPHA: f64 = 0.001;
pub const RANDOM_TEST_MIN_TRADES: usize = 1000;

fn random_verdict(name: &str, res: &SimResult) -> CheckOutcome {
    let st = random_stats(res);
    // Beklenen sonuç ≈ −maliyet: maliyetsiz ortalama 0'dan ayırt edilemez ve net < 0.
    let pass = st.trades >= RANDOM_TEST_MIN_TRADES
        && st.p_value >= RANDOM_TEST_ALPHA
        && st.mean_net_usd < 0.0
        && (st.mean_net_usd + st.mean_cost_usd - st.mean_frictionless_usd).abs() < 1e-6;
    CheckOutcome::new(name, pass, serde_json::to_value(&st).unwrap_or_default())
}

/// Rastgele giriş, `TradeFill`, ±1 tick rastgele yürüyüş.
pub fn check_random_trade_fill(seed: u64) -> CheckOutcome {
    let name = "random_entry_trade_fill";
    let env = match validation_env() {
        Ok(e) => e,
        Err(e) => return CheckOutcome::error(name, e),
    };
    let inst = InstrumentId(0);
    let evs = random_walk_trades(
        seed,
        inst,
        UnixNanos::from_secs(1_700_000_000),
        600_000,
        NANOS_PER_SEC,
        Price(100_000),
    );
    let model = TradeFill {
        limit_rule: LimitRule::Touch,
    };
    let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(RandomEntry::new(
        seed,
        inst,
        0.05,
        10,
        10,
        30 * NANOS_PER_MIN,
    ))];
    match run_sim(&env, &model, SimConfig::default(), evs, &mut strats) {
        Ok(r) => random_verdict(name, &r),
        Err(e) => CheckOutcome::error(name, e),
    }
}

/// Rastgele giriş, `BarFill`, aynı yürüyüşten 1 dakikalık barlar.
pub fn check_random_bar_fill(seed: u64) -> CheckOutcome {
    let name = "random_entry_bar_fill";
    let env = match validation_env() {
        Ok(e) => e,
        Err(e) => return CheckOutcome::error(name, e),
    };
    let inst = InstrumentId(0);
    let trades = random_walk_trades(
        seed,
        inst,
        UnixNanos::from_secs(1_700_000_000),
        6_000_000,
        NANOS_PER_SEC,
        Price(100_000),
    );
    let bars = bars_from_trades(&trades, inst, NANOS_PER_MIN);
    drop(trades);
    let model = BarFill {
        limit_rule: LimitRule::Touch,
    };
    let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(RandomEntry::new(
        seed,
        inst,
        0.2,
        25,
        25,
        12 * 60 * NANOS_PER_MIN,
    ))];
    match run_sim(&env, &model, SimConfig::default(), bars, &mut strats) {
        Ok(r) => random_verdict(name, &r),
        Err(e) => CheckOutcome::error(name, e),
    }
}

/// Gerçek veride rastgele giriş (bilgi amaçlı; veri varsa CLI çalıştırır).
pub fn check_random_on_events(
    name: &str,
    env: &RiskEnv,
    inst: InstrumentId,
    events: Vec<MarketEvent>,
    seed: u64,
    stop_ticks: i64,
) -> CheckOutcome {
    let model = BarFill {
        limit_rule: LimitRule::Touch,
    };
    let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(RandomEntry::new(
        seed,
        inst,
        0.02,
        stop_ticks,
        stop_ticks,
        8 * 60 * NANOS_PER_MIN,
    ))];
    match run_sim(env, &model, SimConfig::default(), events, &mut strats) {
        Ok(r) => random_verdict(name, &r),
        Err(e) => CheckOutcome::error(name, e),
    }
}

fn bar(i: u64, o: i64, h: i64, l: i64, c: i64) -> MarketEvent {
    let open_ts = UnixNanos::from_secs(1_700_000_000).plus(i * NANOS_PER_MIN);
    MarketEvent::bar(
        InstrumentId(0),
        Bar {
            open_ts,
            close_ts: open_ts.plus(NANOS_PER_MIN),
            open: Price(o),
            high: Price(h),
            low: Price(l),
            close: Price(c),
            volume: 1,
            trades: 1,
        },
    )
}

/// Al-tut: brüt = (son kapanış − giriş açılışı) × tick değeri, birebir.
pub fn check_buy_hold(events: Vec<MarketEvent>, env: &RiskEnv) -> CheckOutcome {
    let name = "buy_hold_exact";
    let Some(inst) = events.first().map(|e| e.instrument) else {
        return CheckOutcome::error(name, "veri yok");
    };
    let model = BarFill::default();
    let cfg = SimConfig {
        slippage_override: Some(0),
        ..SimConfig::default()
    };
    let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(BuyHold::new(inst))];
    let first_open = events.get(1).and_then(|e| match &e.payload {
        atlas_core::MarketPayload::Bar(b) => Some(b.open),
        _ => None,
    });
    let last_close = events.last().and_then(atlas_core::MarketEvent::last_price);
    let res = match run_sim(env, &model, cfg, events, &mut strats) {
        Ok(r) => r,
        Err(e) => return CheckOutcome::error(name, e),
    };
    let (Some(o), Some(c), Some(tr), Some(i)) = (
        first_open,
        last_close,
        res.trades.first(),
        env.instruments.by_id(inst),
    ) else {
        return CheckOutcome::error(name, "işlem oluşmadı");
    };
    let expected_gross = i.pnl(o, c, atlas_core::Qty(1));
    let pass = res.trades.len() == 1
        && tr.entry_price == o
        && tr.exit_price == c
        && tr.gross == expected_gross
        && tr.net == expected_gross - tr.fees
        && res.net_pnl() == tr.net;
    CheckOutcome::new(
        name,
        pass,
        serde_json::json!({"entry_open": o, "last_close": c, "gross": tr.gross, "expected_gross": expected_gross, "fees": tr.fees, "net": tr.net, "account_net": res.net_pnl()}),
    )
}

pub fn check_buy_hold_synthetic(seed: u64) -> CheckOutcome {
    let env = match validation_env() {
        Ok(e) => e,
        Err(e) => return CheckOutcome::error("buy_hold_exact", e),
    };
    let trades = random_walk_trades(
        seed,
        InstrumentId(0),
        UnixNanos::from_secs(1_700_000_000),
        200_000,
        NANOS_PER_SEC,
        Price(100_000),
    );
    check_buy_hold(
        bars_from_trades(&trades, InstrumentId(0), NANOS_PER_MIN),
        &env,
    )
}

fn scenario(bars: Vec<MarketEvent>) -> Result<SimResult, String> {
    let env = validation_env()?;
    let model = BarFill::default();
    let cfg = SimConfig {
        slippage_override: Some(0),
        ..SimConfig::default()
    };
    let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(OneShot {
        id: StrategyId::new("scenario"),
        inst: InstrumentId(0),
        side: Side::Buy,
        stop: Price(90),
        target: Price(110),
        done: false,
    })];
    run_sim(&env, &model, cfg, bars, &mut strats).map_err(|e| e.to_string())
}

/// Aynı mumda stop ve hedef → stop.
pub fn check_same_bar_stop_wins() -> CheckOutcome {
    let name = "same_bar_stop_and_target_is_stop";
    match scenario(vec![
        bar(0, 100, 100, 100, 100),
        bar(1, 100, 101, 99, 100),
        bar(2, 100, 115, 85, 100),
        bar(3, 100, 100, 100, 100),
    ]) {
        Ok(r) => {
            let t = r.trades.first();
            let pass = r.trades.len() == 1
                && t.is_some_and(|t| {
                    t.exit_reason == FillReason::StopLoss && t.exit_price == Price(90)
                });
            CheckOutcome::new(name, pass, serde_json::json!({"trades": r.trades}))
        }
        Err(e) => CheckOutcome::error(name, e),
    }
}

/// Gap'te stop dolumu açılıştan.
pub fn check_gap_stop_at_open() -> CheckOutcome {
    let name = "gap_stop_fills_at_open";
    match scenario(vec![
        bar(0, 100, 100, 100, 100),
        bar(1, 100, 101, 99, 100),
        bar(2, 80, 85, 75, 82),
        bar(3, 82, 82, 82, 82),
    ]) {
        Ok(r) => {
            let t = r.trades.first();
            let pass = r.trades.len() == 1
                && t.is_some_and(|t| {
                    t.exit_reason == FillReason::StopLoss && t.exit_price == Price(80)
                });
            CheckOutcome::new(name, pass, serde_json::json!({"trades": r.trades}))
        }
        Err(e) => CheckOutcome::error(name, e),
    }
}

/// Aynı seed → aynı hash; farklı seed → farklı hash.
pub fn check_determinism(seed: u64) -> CheckOutcome {
    let name = "determinism_same_seed_same_bits";
    let env = match validation_env() {
        Ok(e) => e,
        Err(e) => return CheckOutcome::error(name, e),
    };
    let run = |s: u64| -> Result<String, SimError> {
        let inst = InstrumentId(0);
        let evs = random_walk_trades(
            s,
            inst,
            UnixNanos::from_secs(1_700_000_000),
            100_000,
            NANOS_PER_SEC,
            Price(100_000),
        );
        let bars = bars_from_trades(&evs, inst, NANOS_PER_MIN);
        let model = BarFill::default();
        let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(RandomEntry::new(
            s,
            inst,
            0.1,
            8,
            12,
            90 * NANOS_PER_MIN,
        ))];
        run_sim(&env, &model, SimConfig::default(), bars, &mut strats).map(|r| r.hash())
    };
    match (run(seed), run(seed), run(seed + 1)) {
        (Ok(a), Ok(b), Ok(c)) => CheckOutcome::new(
            name,
            a == b && a != c,
            serde_json::json!({"run1": a, "run2": b, "other_seed": c}),
        ),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => CheckOutcome::error(name, e),
    }
}

/// M1 simülasyon doğrulamalarının hepsi.
pub fn all_sim_checks(seed: u64) -> Vec<CheckOutcome> {
    vec![
        check_random_trade_fill(seed),
        check_random_bar_fill(seed),
        check_buy_hold_synthetic(seed),
        check_same_bar_stop_wins(),
        check_gap_stop_at_open(),
        check_determinism(seed),
    ]
}

// ---------------------------------------------------------------------------
// Prop kural uyumu (M1 kabul: "prop kural testleri Arman'ın doldurduğu config'le birebir uyar").
// Yüklenen gerçek `PropRules` üzerinde, kurgusal bir test enstrümanıyla ($1/tick, ücret 0)
// RiskGate + RiskState davranışı config değerleriyle karşılaştırılır.
// ---------------------------------------------------------------------------

const CONFORMANCE_INSTRUMENT: &str = r#"
[[instrument]]
symbol = "CONF"
venue = "binance"
kind = "future"
tick_size = "1"
tick_value_usd = "1"
session = "always"
exposure_weight = "1"
[instrument.costs]
commission_per_contract_usd = "0"
slippage_ticks = "0"
"#;

const CONFORMANCE_POLICY: &str = r#"
[risk]
cooldown_after_stop_minutes = 0
max_risk_per_trade_usd = "1000000000"
max_group_net_contracts = "1000000"
consistency_headroom = "1"
drawdown_near = "0"
stop_slippage_buffer_ticks = 0
min_minutes_before_week_close = 0
"#;

fn conformance_env(rules: &PropRules) -> Result<RiskEnv, String> {
    Ok(RiskEnv {
        rules: rules.clone(),
        policy: RiskPolicy::from_toml_str(CONFORMANCE_POLICY).map_err(|e| e.to_string())?,
        instruments: InstrumentTable::from_toml_str(CONFORMANCE_INSTRUMENT)
            .map_err(|e| e.to_string())?,
        calendar: Calendar::from_toml_str(VALIDATION_CALENDAR).map_err(|e| e.to_string())?,
    })
}

fn conformance_day() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 10, 27).unwrap_or_default()
}

/// Firma gün sıfırlamasından 2 saat sonrası, 2026-10-27 (Salı) — işlem saatleri içinde.
fn conformance_t0(rules: &PropRules) -> UnixNanos {
    let m = (rules.day_reset_time.0 + 120) % 1440;
    atlas_core::calendar::local_to_utc(conformance_day(), m, rules.day_reset_tz)
}

const P0: Price = Price(1_000_000);

fn conf_open(ts: UnixNanos, qty: i32, stop: Price) -> Intent {
    Intent {
        strategy: StrategyId::new("conf"),
        instrument: InstrumentId(0),
        ts,
        action: Action::Open {
            side: Side::Buy,
            qty,
            entry: EntryType::Market,
            stop_loss: stop,
            take_profit: None,
            time_exit: None,
            entry_expiry: None,
        },
    }
}

fn conf_fill(
    ts: UnixNanos,
    side: Side,
    qty: i32,
    price: Price,
    reason: FillReason,
) -> atlas_core::Fill {
    atlas_core::Fill {
        ts,
        strategy: StrategyId::new("conf"),
        instrument: InstrumentId(0),
        side,
        qty,
        price,
        fee: Money::ZERO,
        reason,
    }
}

/// `P0`'dan long açar.
fn conf_enter(env: &RiskEnv, st: &mut RiskState, ts: UnixNanos, qty: i32) {
    st.register_pending(
        StrategyId::new("conf"),
        InstrumentId(0),
        PendingEntry {
            side: Side::Buy,
            qty,
            entry_ref: P0,
            stop: P0 - 1,
        },
    );
    st.on_fill(env, &conf_fill(ts, Side::Buy, qty, P0, FillReason::Entry));
}

fn whole_usd(m: Money) -> i64 {
    m.0 / atlas_core::units::MICROS_PER_USD
}

fn has(evs: &[atlas_core::Event], k: EventKind) -> bool {
    evs.iter().any(|e| e.kind == k)
}

pub fn prop_conformance(rules: &PropRules) -> Vec<CheckOutcome> {
    let mut out = Vec::new();
    let env = match conformance_env(rules) {
        Ok(e) => e,
        Err(e) => return vec![CheckOutcome::error("prop_conformance_env", e)],
    };
    let gate = RiskGate::new(env.clone());
    let t0 = conformance_t0(rules);
    let fresh = |env: &RiskEnv| {
        let mut st = RiskState::new(env, t0);
        st.on_price(env, t0, InstrumentId(0), P0);
        st
    };
    let start = rules.starting_balance;
    let dd = rules.drawdown_amount;
    let no_daily = |r: &PropRules| {
        let mut r = r.clone();
        r.daily_loss_limit = None;
        conformance_env(&r)
    };

    // 1. Başlangıç tabanı
    let st = fresh(&env);
    out.push(CheckOutcome::new(
        "prop_initial_floor",
        st.floor() == start - dd,
        serde_json::json!({"floor": st.floor(), "expected": start - dd}),
    ));

    // 2. Algo izni yoksa her giriş reddedilir; geri kalan testler anlamsız.
    if !rules.algo_trading_allowed {
        let d = gate.check(&st, &conf_open(t0, 1, P0 - 1));
        out.push(CheckOutcome::new(
            "prop_algo_not_allowed_blocks",
            !d.is_approved(),
            serde_json::json!({"verdict": format!("{:?}", d.verdict)}),
        ));
        return out;
    }

    // 3. Kontrat tavanı: N geçer, N+1 düşer
    let n = i32::try_from(rules.max_contracts_milli / 1000).unwrap_or(i32::MAX);
    let ok_n = n >= 1 && gate.check(&st, &conf_open(t0, n, P0 - 1)).is_approved();
    let no_n1 = !gate
        .check(&st, &conf_open(t0, n.saturating_add(1), P0 - 1))
        .is_approved();
    out.push(CheckOutcome::new(
        "prop_max_contracts",
        ok_n && no_n1,
        serde_json::json!({"max": n, "n_approved": ok_n, "n_plus_1_rejected": no_n1}),
    ));

    // 4. Taban ihlali (günlük limit kapalıyken): taban+$1 → yok; taban → var
    match no_daily(rules) {
        Ok(env2) => {
            let mut st = fresh(&env2);
            conf_enter(&env2, &mut st, t0, 1);
            let dist = whole_usd(st.equity(&env2) - st.floor());
            let early = has(
                &st.on_price(&env2, t0, InstrumentId(0), P0 - (dist - 1)),
                EventKind::RiskBreach,
            );
            let at = has(
                &st.on_price(&env2, t0, InstrumentId(0), P0 - dist),
                EventKind::RiskBreach,
            ) && st.kills.any_account_level();
            out.push(CheckOutcome::new(
                "prop_drawdown_breach_exact",
                !early && at,
                serde_json::json!({"distance_usd": dist, "breach_one_dollar_early": early, "breach_at_floor": at}),
            ));
        }
        Err(e) => out.push(CheckOutcome::error("prop_drawdown_breach_exact", e)),
    }

    // 5. Günlük zarar limiti: limit−$1 → yok; limit → var
    match rules.daily_loss_limit {
        Some(limit) if limit < dd => {
            let mut st = fresh(&env);
            conf_enter(&env, &mut st, t0, 1);
            let l = whole_usd(limit);
            let early = has(&st.on_price(&env, t0, InstrumentId(0), P0 - (l - 1)), EventKind::DailyLossLimit);
            let at = has(&st.on_price(&env, t0, InstrumentId(0), P0 - l), EventKind::DailyLossLimit)
                && st.kills.any_account_level();
            out.push(CheckOutcome::new(
                "prop_daily_loss_limit_exact",
                !early && at,
                serde_json::json!({"limit": limit, "early": early, "at_limit": at}),
            ));
        }
        Some(limit) => out.push(CheckOutcome::new(
            "prop_daily_loss_limit_exact",
            true,
            serde_json::json!({"note": "günlük limit ≥ drawdown; taban önce bağlar", "limit": limit}),
        )),
        None => {}
    }

    // 6. Taban davranışı (static / eod_trailing / intraday_trailing) ve kilit seviyesi
    match no_daily(rules) {
        Ok(env3) => {
            let mut st = fresh(&env3);
            conf_enter(&env3, &mut st, t0, 1);
            let gain = whole_usd(dd) / 2;
            st.on_price(&env3, t0, InstrumentId(0), P0 + gain);
            let intraday = st.floor();
            let day2 = t0.plus(atlas_core::units::NANOS_PER_DAY);
            st.on_price(&env3, day2, InstrumentId(0), P0 + gain);
            let after_reset = st.floor();
            let raised = {
                let c = start - dd + Money::from_usd(gain);
                match rules.drawdown_lock_level {
                    Some(l) => c.min(l).max(start - dd),
                    None => c,
                }
            };
            let (exp_i, exp_r) = match rules.drawdown_type {
                DrawdownType::Static => (start - dd, start - dd),
                DrawdownType::EodTrailing => (start - dd, raised),
                DrawdownType::IntradayTrailing => (raised, raised),
            };
            out.push(CheckOutcome::new(
                "prop_drawdown_type_behaviour",
                intraday == exp_i && after_reset == exp_r,
                serde_json::json!({"type": rules.drawdown_type, "intraday": intraday, "expected_intraday": exp_i,
                    "after_reset": after_reset, "expected_after_reset": exp_r}),
            ));
            if let Some(lock) = rules.drawdown_lock_level
                && rules.drawdown_type != DrawdownType::Static
            {
                let big = whole_usd(dd) * 3;
                let day3 = day2.plus(atlas_core::units::NANOS_PER_DAY);
                st.on_price(&env3, day2, InstrumentId(0), P0 + big);
                st.on_price(&env3, day3, InstrumentId(0), P0 + big);
                out.push(CheckOutcome::new(
                    "prop_trailing_lock_level",
                    st.floor() == lock,
                    serde_json::json!({"floor": st.floor(), "lock": lock}),
                ));
            }
        }
        Err(e) => out.push(CheckOutcome::error("prop_drawdown_type_behaviour", e)),
    }

    // 7. Consistency: dün P kâr → bugünkü tavan rP/(1−r)
    if let Some(r) = rules.consistency_ratio_milli {
        match no_daily(rules) {
            Ok(env4) => {
                let mut st = fresh(&env4);
                conf_enter(&env4, &mut st, t0, 1);
                let p = 1000i64;
                st.on_fill(
                    &env4,
                    &conf_fill(t0, Side::Sell, 1, P0 + p, FillReason::TakeProfit),
                );
                st.on_price(
                    &env4,
                    t0.plus(atlas_core::units::NANOS_PER_DAY),
                    InstrumentId(0),
                    P0,
                );
                let cap = st.consistency_cap_today(&env4);
                let expected = Money(Money::from_usd(p).0 * r / (1000 - r));
                out.push(CheckOutcome::new(
                    "prop_consistency_cap",
                    cap == Some(expected),
                    serde_json::json!({"ratio_milli": r, "cap": cap, "expected": expected}),
                ));
            }
            Err(e) => out.push(CheckOutcome::error("prop_consistency_cap", e)),
        }
    }

    // 8. Saat kuralları
    if let Some(m) = rules.no_new_entries_after {
        // t0'dan sonraki gün (durum zamanı geriye gidemez).
        let day = conformance_day().succ_opt().unwrap_or_default();
        let ts = atlas_core::calendar::local_to_utc(day, m.0, rules.rules_tz);
        let st = fresh(&env);
        let at = gate.check(&st, &conf_open(ts, 1, P0 - 1));
        let before = gate.check(
            &st,
            &conf_open(ts.minus(atlas_core::units::NANOS_PER_MIN), 1, P0 - 1),
        );
        out.push(CheckOutcome::new(
            "prop_no_new_entries_after",
            !at.is_approved() && before.is_approved(),
            serde_json::json!({"at": format!("{:?}", at.verdict), "one_min_before": format!("{:?}", before.verdict)}),
        ));
    }
    if let Some(m) = rules.flat_by {
        let ts = atlas_core::calendar::local_to_utc(conformance_day(), m.0, rules.rules_tz);
        let pass =
            env.must_be_flat(ts) && !env.must_be_flat(ts.minus(atlas_core::units::NANOS_PER_MIN));
        out.push(CheckOutcome::new(
            "prop_flat_by",
            pass,
            serde_json::json!({"flat_by_minute": m.0}),
        ));
    }
    out
}

/// Doğrulama için kullanılan para birimi yardımcı (CLI'da gösterim).
pub fn usd(m: Money) -> f64 {
    m.as_usd_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_pass(o: &CheckOutcome) {
        assert!(o.pass, "{}: {}", o.name, o.detail);
    }

    #[test]
    fn random_entry_trade_fill_is_minus_cost() {
        assert_pass(&check_random_trade_fill(42));
    }

    #[test]
    fn random_entry_bar_fill_is_minus_cost() {
        assert_pass(&check_random_bar_fill(42));
    }

    #[test]
    fn buy_hold_exact() {
        assert_pass(&check_buy_hold_synthetic(42));
    }

    #[test]
    fn same_bar_stop_wins() {
        assert_pass(&check_same_bar_stop_wins());
    }

    #[test]
    fn gap_stop_at_open() {
        assert_pass(&check_gap_stop_at_open());
    }

    #[test]
    fn determinism() {
        assert_pass(&check_determinism(42));
    }

    /// Model sapmalı olsaydı test yakalardı: limitler dokunmayla değil içinden geçince dolarsa
    /// hedefler sistematik olarak 1 tick kötü dolar → maliyetsiz ortalama negatif, test düşer.
    #[test]
    fn random_test_has_power_against_biased_model() {
        let env = validation_env().unwrap();
        let inst = InstrumentId(0);
        let evs = random_walk_trades(
            42,
            inst,
            UnixNanos::from_secs(1_700_000_000),
            600_000,
            NANOS_PER_SEC,
            Price(100_000),
        );
        let model = TradeFill::default(); // TradeThrough: bilinçli karamsar sapma
        let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(RandomEntry::new(
            42,
            inst,
            0.05,
            10,
            3,
            30 * NANOS_PER_MIN,
        ))];
        let r = run_sim(&env, &model, SimConfig::default(), evs, &mut strats).unwrap();
        let st = random_stats(&r);
        assert!(st.p_value < RANDOM_TEST_ALPHA, "{st:?}");
        assert!(st.mean_frictionless_usd < 0.0);
    }

    const CONF_BASE: &str = r#"
firm = "TEST (kurgusal)"
account_label = "t"
starting_balance_usd = "50000"
profit_target_usd = "3000"
drawdown_type = "eod_trailing"
drawdown_amount_usd = "2000"
drawdown_lock_level_usd = "50000"
daily_loss_limit_usd = "1000"
daily_loss_reference = "day_start_balance"
day_reset_tz = "America/Chicago"
day_reset_time = "17:00"
consistency_ratio = "0.5"
max_contracts = "5"
rules_tz = "America/Chicago"
no_new_entries_after = "15:00"
flat_by = "15:10"
weekend_holding_allowed = false
news_blackout_minutes_before = "2"
news_blackout_minutes_after = "2"
algo_trading_allowed = true
"#;

    #[test]
    fn conformance_passes_for_all_drawdown_types() {
        for (ty, lock) in [
            ("eod_trailing", "50000"),
            ("intraday_trailing", "none"),
            ("intraday_trailing", "50500"),
            ("static", "none"),
        ] {
            let t = CONF_BASE
                .replace("= \"eod_trailing\"", &format!("= \"{ty}\""))
                .replace(
                    "lock_level_usd = \"50000\"",
                    &format!("lock_level_usd = \"{lock}\""),
                );
            let res = prop_conformance(&PropRules::from_toml_str(&t).unwrap());
            assert!(res.len() >= 6, "{ty}: {res:?}");
            for c in &res {
                assert!(c.pass, "{ty}/{lock} {}: {}", c.name, c.detail);
            }
        }
        // Gece yarısı sıfırlanan, statik, saat kuralsız bir yapı
        let t = CONF_BASE
            .replace("= \"eod_trailing\"", "= \"static\"")
            .replace("lock_level_usd = \"50000\"", "lock_level_usd = \"none\"")
            .replace("day_reset_time = \"17:00\"", "day_reset_time = \"00:00\"")
            .replace(
                "day_reset_tz = \"America/Chicago\"",
                "day_reset_tz = \"Europe/Prague\"",
            )
            .replace(
                "no_new_entries_after = \"15:00\"",
                "no_new_entries_after = \"none\"",
            )
            .replace("flat_by = \"15:10\"", "flat_by = \"none\"")
            .replace(
                "consistency_ratio = \"0.5\"",
                "consistency_ratio = \"none\"",
            );
        for c in prop_conformance(&PropRules::from_toml_str(&t).unwrap()) {
            assert!(c.pass, "midnight-static {}: {}", c.name, c.detail);
        }
        // Algo yasaksa yalnız engelleme testi koşar ve geçer
        let t = CONF_BASE.replace(
            "algo_trading_allowed = true",
            "algo_trading_allowed = false",
        );
        let res = prop_conformance(&PropRules::from_toml_str(&t).unwrap());
        assert!(res.iter().all(|c| c.pass));
        assert!(res.iter().any(|c| c.name == "prop_algo_not_allowed_blocks"));
    }

    #[test]
    fn out_of_order_data_is_an_error() {
        let env = validation_env().unwrap();
        let mut strats: Vec<Box<dyn Strategy>> = vec![];
        let r = run_sim(
            &env,
            &BarFill::default(),
            SimConfig::default(),
            vec![bar(2, 1, 1, 1, 1), bar(1, 1, 1, 1, 1)],
            &mut strats,
        );
        assert!(matches!(r, Err(SimError::OutOfOrder { .. })));
    }

    #[test]
    fn feed_down_is_not_a_price() {
        let env = validation_env().unwrap();
        let mut evs = vec![bar(0, 100, 100, 100, 100), bar(1, 100, 101, 99, 100)];
        evs.push(MarketEvent {
            ts: evs[1].ts.plus(1),
            instrument: InstrumentId(0),
            payload: atlas_core::MarketPayload::Feed(atlas_core::FeedStatus::Down {
                reason: "bağlantı koptu".into(),
            }),
        });
        evs.push(bar(3, 100, 100, 100, 100));
        let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(OneShot {
            id: StrategyId::new("s"),
            inst: InstrumentId(0),
            side: Side::Buy,
            stop: Price(90),
            target: Price(110),
            done: false,
        })];
        let r = run_sim(
            &env,
            &BarFill::default(),
            SimConfig {
                slippage_override: Some(0),
                ..SimConfig::default()
            },
            evs,
            &mut strats,
        )
        .unwrap();
        // Kesinti pozisyonu kapatmaz, kayıp üretmez.
        assert_eq!(r.trades.len(), 1);
        assert_eq!(r.trades[0].exit_reason, FillReason::EndOfData);
        assert_eq!(r.trades[0].gross, Money::ZERO);
        assert!(
            r.events
                .iter()
                .any(|e| e.kind == atlas_core::EventKind::FeedDown)
        );
    }
}
