//! RiskGate + RiskState davranış testleri ve özellik (proptest) testleri.

use crate::config::tests::{TEST_POLICY, TEST_PROP};
use crate::*;
use atlas_core::calendar::{Calendar, parse_rfc3339};
use atlas_core::units::NANOS_PER_MIN;
use atlas_core::{
    Action, EntryType, Event, EventKind, Fill, FillReason, Instrument, InstrumentId,
    InstrumentTable, Intent, Money, Price, Side, StrategyId, UnixNanos,
};
use proptest::prelude::*;

/// Testlere özel kurgusal maliyetler (gerçek broker ücreti değildir).
const TEST_INSTRUMENTS: &str = r#"
[[instrument]]
symbol = "NQ"
venue = "cme"
kind = "future"
tick_size = "0.25"
tick_value_usd = "5"
session = "cme_globex"
exposure_group = "us_tech"
exposure_weight = "1"
[instrument.costs]
commission_per_contract_usd = "2"
slippage_ticks = "1"

[[instrument]]
symbol = "MNQ"
venue = "cme"
kind = "future"
tick_size = "0.25"
tick_value_usd = "0.5"
session = "cme_globex"
exposure_group = "us_tech"
exposure_weight = "0.1"
[instrument.costs]
commission_per_contract_usd = "0.5"
slippage_ticks = "1"

[[instrument]]
symbol = "GC"
venue = "cme"
kind = "future"
tick_size = "0.1"
tick_value_usd = "10"
session = "cme_globex"
exposure_group = "metals"
exposure_weight = "1"
[instrument.costs]
commission_per_contract_usd = "2"
slippage_ticks = "1"
"#;

const TEST_CAL: &str = r#"
[[session]]
name = "cme_globex"
tz = "America/Chicago"
open = "sun 17:00"
close = "fri 16:00"
daily_break = ["16:00", "17:00"]
"#;

fn env_with(prop: &str) -> RiskEnv {
    RiskEnv {
        rules: PropRules::from_toml_str(prop).unwrap(),
        policy: RiskPolicy::from_toml_str(TEST_POLICY).unwrap(),
        instruments: InstrumentTable::from_toml_str(TEST_INSTRUMENTS).unwrap(),
        calendar: Calendar::from_toml_str(TEST_CAL).unwrap(),
    }
}

fn env() -> RiskEnv {
    env_with(TEST_PROP)
}

fn t(s: &str) -> UnixNanos {
    parse_rfc3339(s).unwrap()
}

/// Salı 2026-10-27 10:00 CT (CDT, UTC-5).
fn tue10() -> UnixNanos {
    t("2026-10-27T15:00:00Z")
}

fn id(env: &RiskEnv, s: &str) -> InstrumentId {
    env.instruments.get(s).unwrap().id
}

fn open(
    strategy: &str,
    inst: InstrumentId,
    ts: UnixNanos,
    side: Side,
    qty: i32,
    stop: i64,
) -> Intent {
    Intent {
        strategy: StrategyId::new(strategy),
        instrument: inst,
        ts,
        action: Action::Open {
            side,
            qty,
            entry: EntryType::Market,
            stop_loss: Price(stop),
            take_profit: None,
            time_exit: None,
            entry_expiry: None,
        },
    }
}

fn setup() -> (RiskEnv, RiskState, InstrumentId) {
    let env = env();
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    (env, s, nq)
}

fn reason(d: &Decision) -> Option<&RejectReason> {
    match &d.verdict {
        Verdict::Approve => None,
        Verdict::Reject(r) => Some(r),
    }
}

fn fill(
    strategy: &str,
    inst: InstrumentId,
    ts: UnixNanos,
    side: Side,
    qty: i32,
    px: i64,
    reason: FillReason,
) -> Fill {
    Fill {
        ts,
        strategy: StrategyId::new(strategy),
        instrument: inst,
        side,
        qty,
        price: Price(px),
        fee: Money::from_usd(2) * i64::from(qty),
        reason,
    }
}

/// Onaylı girişi kaydedip dolduran yardımcı.
fn enter(env: &RiskEnv, s: &mut RiskState, i: &Intent, px: i64) -> Vec<Event> {
    let Action::Open {
        side,
        qty,
        stop_loss,
        ..
    } = i.action.clone()
    else {
        unreachable!()
    };
    s.register_pending(
        i.strategy.clone(),
        i.instrument,
        PendingEntry {
            side,
            qty,
            entry_ref: Price(px),
            stop: stop_loss,
        },
    );
    s.on_fill(
        env,
        &fill(
            &i.strategy.0,
            i.instrument,
            i.ts,
            side,
            qty,
            px,
            FillReason::Entry,
        ),
    )
}

#[test]
fn approves_reasonable_entry() {
    let (env, s, nq) = setup();
    let g = RiskGate::new(env);
    // 20 tick stop × $5 = $100 + tampon 2×5 + ücret 4 = $114 ≤ $400
    let d = g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_980));
    assert!(d.is_approved(), "{d:?}");
}

#[test]
fn risk_reducing_always_passes_even_when_killed() {
    let (env, mut s, nq) = setup();
    s.kills.trigger(KillScope::Global, "test", tue10());
    let g = RiskGate::new(env);
    let close = Intent {
        strategy: StrategyId::new("a"),
        instrument: nq,
        ts: tue10(),
        action: Action::Close { reason: "x".into() },
    };
    assert!(g.check(&s, &close).is_approved());
    let d = g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_980));
    assert!(matches!(reason(&d), Some(RejectReason::Killed(_))));
}

#[test]
fn strategy_kill_and_pause() {
    let (env, mut s, nq) = setup();
    s.kills
        .trigger(KillScope::Strategy(StrategyId::new("a")), "x", tue10());
    s.paused.insert(StrategyId::new("b"));
    let g = RiskGate::new(env);
    assert!(matches!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_980))),
        Some(RejectReason::Killed(_))
    ));
    assert_eq!(
        reason(&g.check(&s, &open("b", nq, tue10(), Side::Buy, 1, 83_980))),
        Some(&RejectReason::Paused)
    );
    assert!(
        g.check(&s, &open("c", nq, tue10(), Side::Buy, 1, 83_980))
            .is_approved()
    );
}

#[test]
fn session_and_rule_hours() {
    let (env, s, nq) = setup();
    let g = RiskGate::new(env);
    // Cumartesi
    let sat = t("2026-10-31T15:00:00Z");
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, sat, Side::Buy, 1, 83_980))),
        Some(&RejectReason::SessionClosed)
    );
    // Salı 15:30 CT: no_new_entries_after 15:00
    let late = t("2026-10-27T20:30:00Z");
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, late, Side::Buy, 1, 83_980))),
        Some(&RejectReason::EntriesClosedByRules)
    );
    // Salı 17:30 CT: yeni gün, açık
    let evening = t("2026-10-27T22:30:00Z");
    assert!(
        g.check(&s, &open("a", nq, evening, Side::Buy, 1, 83_980))
            .is_approved()
    );
    assert!(g.env.must_be_flat(t("2026-10-27T20:15:00Z")));
    assert!(!g.env.must_be_flat(t("2026-10-27T20:05:00Z")));
}

#[test]
fn weekend_lead_blocks_entries() {
    let (env, s, nq) = setup();
    let g = RiskGate::new(env);
    // Perşembe 17:30 CT → Cuma 16:00 kapanışa çok var
    assert!(
        g.check(
            &s,
            &open("a", nq, t("2026-10-29T22:30:00Z"), Side::Buy, 1, 83_980)
        )
        .is_approved()
    );
    // Cuma 14:30 CT → 90 dk kala, ama 15:00 kuralı yok... 60 dk eşiği: 15:01 sonrası no-new zaten
    // Kural saatlerini kapatarak yalnız hafta sonu tamponunu test et.
    let prop = TEST_PROP
        .replace(
            "no_new_entries_after = \"15:00\"",
            "no_new_entries_after = \"none\"",
        )
        .replace("flat_by = \"15:10\"", "flat_by = \"none\"");
    let g2 = RiskGate::new(env_with(&prop));
    let fri_1530 = t("2026-10-30T20:30:00Z");
    assert_eq!(
        reason(&g2.check(&s, &open("a", nq, fri_1530, Side::Buy, 1, 83_980))),
        Some(&RejectReason::NearWeekClose)
    );
    assert!(
        g2.env
            .near_week_close(nq, t("2026-10-30T20:56:00Z"), WEEKEND_FLATTEN_LEAD_MIN)
    );
}

#[test]
fn news_blackout() {
    let (env, mut s, nq) = setup();
    s.news.push(tue10().plus(NANOS_PER_MIN));
    let g = RiskGate::new(env);
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_980))),
        Some(&RejectReason::NewsBlackout)
    );
    let later = tue10().plus(4 * NANOS_PER_MIN);
    assert!(
        g.check(&s, &open("a", nq, later, Side::Buy, 1, 83_980))
            .is_approved()
    );
}

#[test]
fn validity_checks() {
    let (env, s, nq) = setup();
    let g = RiskGate::new(env);
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 84_010))),
        Some(&RejectReason::BadStop)
    );
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Sell, 1, 83_990))),
        Some(&RejectReason::BadStop)
    );
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 0, 83_980))),
        Some(&RejectReason::BadQty)
    );
    let mut i = open("a", nq, tue10(), Side::Buy, 1, 83_980);
    if let Action::Open { take_profit, .. } = &mut i.action {
        *take_profit = Some(Price(83_990));
    }
    assert_eq!(reason(&g.check(&s, &i)), Some(&RejectReason::BadTarget));
    let gc = id(&g.env, "GC");
    assert_eq!(
        reason(&g.check(&s, &open("a", gc, tue10(), Side::Buy, 1, 1))),
        Some(&RejectReason::NoPrice)
    );
}

#[test]
fn contract_cap_uses_weights() {
    let (env, mut s, nq) = setup();
    let mnq = id(&env, "MNQ");
    s.on_price(&env, tue10(), mnq, Price(84_000));
    let g = RiskGate::new(env.clone());
    // 6 NQ > 5
    assert!(matches!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 6, 83_998))),
        Some(RejectReason::MaxContracts { .. })
    ));
    // 4 NQ girildi, 10 MNQ = 1 NQ eşdeğeri → tam 5: geçer; 11 MNQ: düşer
    let i = open("a", nq, tue10(), Side::Buy, 4, 83_995);
    assert!(g.check(&s, &i).is_approved());
    enter(&env, &mut s, &i, 84_000);
    assert!(
        g.check(&s, &open("b", mnq, tue10(), Side::Buy, 10, 83_990))
            .is_approved()
    );
    assert!(matches!(
        reason(&g.check(&s, &open("b", mnq, tue10(), Side::Buy, 11, 83_990))),
        Some(RejectReason::MaxContracts { .. })
    ));
    // aynı strateji+enstrümana ikinci giriş yok
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_990))),
        Some(&RejectReason::AlreadyExposed)
    );
}

#[test]
fn group_net_exposure() {
    let prop = TEST_PROP.replace("max_contracts = \"5\"", "max_contracts = \"20\"");
    let env = env_with(&prop);
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    let g = RiskGate::new(env.clone());
    let i = open("a", nq, tue10(), Side::Buy, 3, 83_995);
    enter(&env, &mut s, &i, 84_000);
    let i2 = open("b", nq, tue10(), Side::Buy, 2, 83_995);
    enter(&env, &mut s, &i2, 84_000);
    // net 5 long; aynı yönde 1 daha → 6 > 5
    assert!(matches!(
        reason(&g.check(&s, &open("c", nq, tue10(), Side::Buy, 1, 83_995))),
        Some(RejectReason::GroupExposure { .. })
    ));
    // ters yönde net azalır → geçer
    assert!(
        g.check(&s, &open("c", nq, tue10(), Side::Sell, 1, 84_005))
            .is_approved()
    );
}

#[test]
fn risk_per_trade_cap() {
    let (env, s, nq) = setup();
    let g = RiskGate::new(env);
    // 100 tick × $5 = $500 > $400
    assert!(matches!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_900))),
        Some(RejectReason::RiskPerTrade { .. })
    ));
}

#[test]
fn cooldown_after_stop_same_direction_only() {
    let (env, mut s, nq) = setup();
    let i = open("a", nq, tue10(), Side::Buy, 1, 83_980);
    enter(&env, &mut s, &i, 84_000);
    let ts = tue10().plus(NANOS_PER_MIN);
    s.on_fill(
        &env,
        &fill("a", nq, ts, Side::Sell, 1, 83_980, FillReason::StopLoss),
    );
    let g = RiskGate::new(env);
    let soon = ts.plus(5 * NANOS_PER_MIN);
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, soon, Side::Buy, 1, 83_960))),
        Some(&RejectReason::Cooldown)
    );
    assert!(
        g.check(&s, &open("a", nq, soon, Side::Sell, 1, 84_010))
            .is_approved()
    );
    let later = ts.plus(16 * NANOS_PER_MIN);
    assert!(
        g.check(&s, &open("a", nq, later, Side::Buy, 1, 83_960))
            .is_approved()
    );
}

#[test]
fn realized_pnl_and_fees() {
    let (env, mut s, nq) = setup();
    let i = open("a", nq, tue10(), Side::Buy, 2, 83_980);
    enter(&env, &mut s, &i, 84_000);
    assert_eq!(s.balance, Money::from_usd(50_000 - 4));
    s.on_price(&env, tue10(), nq, Price(84_010));
    // 10 tick × 2 × $5 = $100 açık kâr
    assert_eq!(s.equity(&env), Money::from_usd(50_000 - 4 + 100));
    s.on_fill(
        &env,
        &fill(
            "a",
            nq,
            tue10(),
            Side::Sell,
            2,
            84_010,
            FillReason::TakeProfit,
        ),
    );
    assert_eq!(s.balance, Money::from_usd(50_000 - 4 + 100 - 4));
    assert!(s.positions().is_empty());
}

/// EOD trailing: HWM yalnız gün kapanışında yükselir; taban kilit seviyesinde durur.
#[test]
fn eod_trailing_floor_and_lock() {
    let (env, mut s, nq) = setup();
    assert_eq!(s.floor(), Money::from_usd(48_000));
    let i = open("a", nq, tue10(), Side::Buy, 1, 83_980);
    enter(&env, &mut s, &i, 84_000);
    // Gün içi +$1000 açık kâr: EOD trailing'de taban gün içinde değişmez
    s.on_price(&env, tue10(), nq, Price(84_200));
    assert_eq!(s.floor(), Money::from_usd(48_000));
    // Gün kapanışı (17:00 CT) sonrası: HWM = equity ≈ 50_998 → taban 48_998
    let next_day = t("2026-10-27T22:05:00Z");
    let ev = s.on_price(&env, next_day, nq, Price(84_200));
    assert!(ev.iter().any(|e| e.kind == EventKind::DayRollover));
    assert_eq!(s.floor(), Money::from_usd(48_998));
    // Büyük kâr: taban kilit seviyesi 50_000'de durur
    s.on_price(&env, next_day, nq, Price(84_800));
    let day3 = t("2026-10-28T22:05:00Z");
    s.on_price(&env, day3, nq, Price(84_800));
    assert_eq!(s.floor(), Money::from_usd(50_000));
    // taban asla düşmez
    s.on_price(&env, day3, nq, Price(84_700));
    let day4 = t("2026-10-29T22:05:00Z");
    s.on_price(&env, day4, nq, Price(84_700));
    assert_eq!(s.floor(), Money::from_usd(50_000));
}

#[test]
fn intraday_trailing_moves_with_equity() {
    let prop = TEST_PROP
        .replace("\"eod_trailing\"", "\"intraday_trailing\"")
        .replace(
            "drawdown_lock_level_usd = \"50000\"",
            "drawdown_lock_level_usd = \"none\"",
        );
    let env = env_with(&prop);
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    let i = open("a", nq, tue10(), Side::Buy, 1, 83_980);
    enter(&env, &mut s, &i, 84_000);
    s.on_price(&env, tue10(), nq, Price(84_100)); // +$500
    assert_eq!(s.floor(), Money::from_usd(48_000 - 2 + 500));
    s.on_price(&env, tue10(), nq, Price(84_050));
    assert_eq!(s.floor(), Money::from_usd(48_000 - 2 + 500));
}

#[test]
fn static_floor_never_moves() {
    let prop = TEST_PROP.replace("\"eod_trailing\"", "\"static\"");
    let env = env_with(&prop);
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    let i = open("a", nq, tue10(), Side::Buy, 1, 83_980);
    s.on_price(&env, tue10(), nq, Price(84_000));
    enter(&env, &mut s, &i, 84_000);
    s.on_price(&env, tue10(), nq, Price(85_000));
    s.on_price(&env, t("2026-10-28T22:05:00Z"), nq, Price(85_000));
    assert_eq!(s.floor(), Money::from_usd(48_000));
}

#[test]
fn breach_triggers_account_kill() {
    let prop = TEST_PROP.replace(
        "daily_loss_limit_usd = \"1000\"",
        "daily_loss_limit_usd = \"none\"",
    );
    let env = env_with(&prop);
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    let i = open("a", nq, tue10(), Side::Buy, 4, 83_990);
    enter(&env, &mut s, &i, 84_000);
    // 4 × 100 tick × $5 = $2000 kayıp (+ücret) → taban 48_000 delinir
    let ev = s.on_price(&env, tue10(), nq, Price(83_900));
    assert!(ev.iter().any(|e| e.kind == EventKind::RiskBreach));
    assert!(ev.iter().any(|e| e.kind == EventKind::KillSwitch));
    assert!(s.kills.any_account_level());
    // tekrar fiyat → yeni kill olayı yok (dedupe)
    let ev2 = s.on_price(&env, tue10(), nq, Price(83_890));
    assert!(!ev2.iter().any(|e| e.kind == EventKind::KillSwitch));
}

#[test]
fn daily_loss_limit_kills_and_near_alert_once() {
    let (env, mut s, nq) = setup();
    let i = open("a", nq, tue10(), Side::Buy, 2, 83_990);
    enter(&env, &mut s, &i, 84_000);
    // 2 × 60 tick × $5 = $600 + 4 ücret: daha limit değil
    let ev = s.on_price(&env, tue10(), nq, Price(83_940));
    assert!(!ev.iter().any(|e| e.kind == EventKind::DailyLossLimit));
    // 2 × 100 tick × $5 = $1000 → limit
    let ev = s.on_price(&env, tue10(), nq, Price(83_900));
    assert!(ev.iter().any(|e| e.kind == EventKind::DailyLossLimit));
    assert!(s.kills.any_account_level());
}

#[test]
fn drawdown_near_alert_is_once_per_day() {
    let prop = TEST_PROP.replace(
        "daily_loss_limit_usd = \"1000\"",
        "daily_loss_limit_usd = \"none\"",
    );
    let env = env_with(&prop);
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    let i = open("a", nq, tue10(), Side::Buy, 3, 83_990);
    enter(&env, &mut s, &i, 84_000);
    // taban 48_000, yakınlık %25 × 2000 = $500 → equity ≤ 48_500 : 3×100 tick ×5 = 1500
    let ev = s.on_price(&env, tue10(), nq, Price(83_900));
    assert!(ev.iter().any(|e| e.kind == EventKind::PropDrawdownNear));
    let ev = s.on_price(&env, tue10(), nq, Price(83_899));
    assert!(!ev.iter().any(|e| e.kind == EventKind::PropDrawdownNear));
}

#[test]
fn floor_room_rejects_when_committed_risk_too_large() {
    let prop = TEST_PROP
        .replace(
            "daily_loss_limit_usd = \"1000\"",
            "daily_loss_limit_usd = \"none\"",
        )
        .replace("max_contracts = \"5\"", "max_contracts = \"50\"");
    let mut env = env_with(&prop);
    env.policy = RiskPolicy::from_toml_str(&TEST_POLICY.replace("\"400\"", "\"5000\"").replace(
        "max_group_net_contracts = \"5\"",
        "max_group_net_contracts = \"50\"",
    ))
    .unwrap();
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    let g = RiskGate::new(env.clone());
    // 4 × 90 tick... toplam riski $2000'a yaklaştır
    let i = open("a", nq, tue10(), Side::Buy, 4, 83_910); // 4×(90+2)×5 = 1840 + 16 ücret
    assert!(g.check(&s, &i).is_approved());
    enter(&env, &mut s, &i, 84_000);
    let d = g.check(&s, &open("b", nq, tue10(), Side::Buy, 1, 83_970));
    assert!(
        matches!(reason(&d), Some(RejectReason::FloorRoom { .. })),
        "{d:?}"
    );
}

#[test]
fn daily_room_rejects() {
    let (env, mut s, nq) = setup();
    let g = RiskGate::new(env.clone());
    // Her biri 1 NQ, stop 60 tick: (60+2)×5 + 4 = $314. Üç tanesi = $942 taahhüt.
    for name in ["a", "b", "c"] {
        let i = open(name, nq, tue10(), Side::Buy, 1, 83_940);
        assert!(g.check(&s, &i).is_approved(), "{name}");
        enter(&env, &mut s, &i, 84_000);
    }
    // Günlük oda: 1000 − 6 (ücret) − 942 = 52 < 314
    let d = g.check(&s, &open("d", nq, tue10(), Side::Buy, 1, 83_940));
    assert!(
        matches!(reason(&d), Some(RejectReason::DailyLossRoom { .. })),
        "{d:?}"
    );
}

#[test]
fn consistency_forward_cap() {
    let (env, mut s, nq) = setup();
    let g = RiskGate::new(env.clone());
    // Gün 1: +$1000 realize
    let i = open("a", nq, tue10(), Side::Buy, 2, 83_980);
    enter(&env, &mut s, &i, 84_000);
    s.on_fill(
        &env,
        &fill(
            "a",
            nq,
            tue10(),
            Side::Sell,
            2,
            84_100,
            FillReason::TakeProfit,
        ),
    );
    assert_eq!(s.consistency_cap_today(&env), None); // önceki gün kârı yok (bugün sayılır)
    // Gün 2'ye geç
    let d2 = t("2026-10-28T15:00:00Z");
    s.on_price(&env, d2, nq, Price(84_100));
    let prior = s.prior_profit(&env);
    assert_eq!(prior, Money::from_usd(1000 - 8));
    // r = 0.5 → tavan = P
    assert_eq!(s.consistency_cap_today(&env), Some(prior));
    // Bugün tavanın %80'ine yaklaş: 2 × 80 tick × $5 = $800 > 0.8 × 992
    let i = open("b", nq, d2, Side::Buy, 2, 84_080);
    enter(&env, &mut s, &i, 84_100);
    s.on_price(&env, d2, nq, Price(84_180));
    assert!(s.tighten_stop(&StrategyId::new("b"), nq, Price(84_170)));
    let d = g.check(&s, &open("c", nq, d2, Side::Buy, 1, 84_170));
    assert!(
        matches!(reason(&d), Some(RejectReason::ConsistencyHeadroom { .. })),
        "{d:?}"
    );
    assert!(
        d.events
            .iter()
            .any(|e| e.kind == EventKind::ConsistencyHeadroom)
    );
}

#[test]
fn consistency_check_on_closed_days() {
    let (env, mut s, nq) = setup();
    // gün 1 +$500, gün 2 +$100 → max/total = 500/600 > 0.5 → ihlal
    let i = open("a", nq, tue10(), Side::Buy, 1, 83_980);
    enter(&env, &mut s, &i, 84_000);
    s.on_fill(
        &env,
        &fill(
            "a",
            nq,
            tue10(),
            Side::Sell,
            1,
            84_100,
            FillReason::TakeProfit,
        ),
    );
    let d2 = t("2026-10-28T15:00:00Z");
    s.on_price(&env, d2, nq, Price(84_100));
    let i = open("a", nq, d2, Side::Buy, 1, 84_080);
    enter(&env, &mut s, &i, 84_100);
    s.on_fill(
        &env,
        &fill("a", nq, d2, Side::Sell, 1, 84_120, FillReason::TakeProfit),
    );
    s.on_price(&env, t("2026-10-29T15:00:00Z"), nq, Price(84_120));
    assert_eq!(s.daily_profit().len(), 2);
    assert!(!s.consistency_ok(&env));
}

#[test]
fn trailing_stop_only_tightens() {
    let (env, mut s, nq) = setup();
    let i = open("a", nq, tue10(), Side::Buy, 1, 83_980);
    enter(&env, &mut s, &i, 84_000);
    let sid = StrategyId::new("a");
    assert!(s.tighten_stop(&sid, nq, Price(83_990)));
    assert!(!s.tighten_stop(&sid, nq, Price(83_950)));
    assert_eq!(s.positions().values().next().unwrap().stop, Price(83_990));
}

#[test]
fn algo_not_allowed_blocks_everything() {
    let prop = TEST_PROP.replace(
        "algo_trading_allowed = true",
        "algo_trading_allowed = false",
    );
    let env = env_with(&prop);
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    let g = RiskGate::new(env);
    assert_eq!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_980))),
        Some(&RejectReason::AlgoNotAllowed)
    );
}

#[test]
fn missing_costs_reject_instead_of_guessing() {
    let mut env = env();
    let nq_sym = "NQ";
    let mut items: Vec<Instrument> = env.instruments.iter().cloned().collect();
    for it in &mut items {
        if it.symbol == nq_sym {
            it.costs = Default::default();
        }
    }
    env.instruments = InstrumentTable::from_instruments(items);
    let nq = id(&env, "NQ");
    let mut s = RiskState::new(&env, tue10());
    s.on_price(&env, tue10(), nq, Price(84_000));
    let g = RiskGate::new(env);
    assert!(matches!(
        reason(&g.check(&s, &open("a", nq, tue10(), Side::Buy, 1, 83_980))),
        Some(RejectReason::CostMissing(_))
    ));
}

// ---- Özellik testi: hiçbir Intent dizisi limiti aşan pozisyon üretemez ----

#[derive(Debug, Clone)]
enum Step {
    Open {
        strat: u8,
        inst: u8,
        buy: bool,
        qty: i32,
        stop_dist: i64,
    },
    Close {
        strat: u8,
        inst: u8,
    },
    Move {
        inst: u8,
        delta: i64,
    },
    Wait {
        minutes: u16,
    },
}

fn step() -> impl proptest::strategy::Strategy<Value = Step> {
    prop_oneof![
        4 => (0u8..4, 0u8..3, any::<bool>(), -2i32..30, -5i64..200).prop_map(|(strat, inst, buy, qty, stop_dist)| Step::Open { strat, inst, buy, qty, stop_dist }),
        2 => (0u8..4, 0u8..3).prop_map(|(strat, inst)| Step::Close { strat, inst }),
        4 => (0u8..3, -150i64..150).prop_map(|(inst, delta)| Step::Move { inst, delta }),
        1 => (1u16..600).prop_map(|minutes| Step::Wait { minutes }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn no_intent_sequence_exceeds_limits(steps in proptest::collection::vec(step(), 1..120)) {
        let env = env();
        let g = RiskGate::new(env.clone());
        let syms = ["NQ", "MNQ", "GC"];
        let ids: Vec<InstrumentId> = syms.iter().map(|s| id(&env, s)).collect();
        let mut now = tue10();
        let mut s = RiskState::new(&env, now);
        let mut px = [Price(84_000), Price(84_000), Price(26_000)];
        for (k, i) in ids.iter().enumerate() {
            s.on_price(&env, now, *i, px[k]);
        }
        for st in steps {
            match st {
                Step::Open { strat, inst, buy, qty, stop_dist } => {
                    let side = if buy { Side::Buy } else { Side::Sell };
                    let k = inst as usize;
                    let stop = px[k].0 - stop_dist * i64::from(side.sign());
                    let intent = open(&format!("s{strat}"), ids[k], now, side, qty, stop);
                    let committed_before = s.committed_risk(&env).unwrap();
                    let equity = s.equity(&env);
                    let d = g.check(&s, &intent);
                    if d.is_approved() {
                        let inst_ref = env.instruments.by_id(ids[k]).unwrap();
                        let risk = entry_risk(inst_ref, side, qty, px[k], Price(stop), env.policy.stop_slippage_buffer_ticks()).unwrap();
                        // Onay anında: işlem başı risk, tabana oda ve kontrat tavanı sağlanır.
                        prop_assert!(qty > 0);
                        prop_assert!(risk <= env.policy.max_risk_per_trade());
                        prop_assert!(risk < equity - s.floor() - committed_before);
                        prop_assert!(!s.kills.any_account_level());
                        enter(&env, &mut s, &intent, px[k].0);
                    }
                }
                Step::Close { strat, inst } => {
                    let k = inst as usize;
                    let sid = StrategyId::new(format!("s{strat}"));
                    if let Some(p) = s.positions().get(&(sid.clone(), ids[k])).cloned() {
                        s.on_fill(&env, &fill(&sid.0, ids[k], now, p.side.opposite(), p.qty, px[k].0, FillReason::Close));
                    }
                }
                Step::Move { inst, delta } => {
                    let k = inst as usize;
                    px[k] = Price((px[k].0 + delta).max(1));
                    s.on_price(&env, now, ids[k], px[k]);
                    // Stop'a değen pozisyonları stop fiyatından kapat.
                    let hits: Vec<_> = s.positions().iter()
                        .filter(|((_, i), p)| *i == ids[k] && match p.side { Side::Buy => px[k] <= p.stop, Side::Sell => px[k] >= p.stop })
                        .map(|((sid, i), p)| (sid.clone(), *i, p.clone()))
                        .collect();
                    for (sid, i, p) in hits {
                        s.on_fill(&env, &fill(&sid.0, i, now, p.side.opposite(), p.qty, px[k].0, FillReason::StopLoss));
                    }
                }
                Step::Wait { minutes } => {
                    now = now.plus(u64::from(minutes) * NANOS_PER_MIN);
                    s.evaluate(&env, now);
                }
            }
            // Her adımdan sonra: ağırlıklı kontrat ve grup net tavanı aşılmaz.
            prop_assert!(s.weighted_contracts_milli(&env) <= env.rules.max_contracts_milli);
            for grp in ["us_tech", "metals"] {
                prop_assert!(s.group_net_milli(&env, grp).abs() <= env.policy.max_group_net_milli());
            }
            // Strateji+enstrüman başına en fazla bir pozisyon.
            for p in s.positions().values() {
                prop_assert!(p.qty > 0);
            }
        }
    }
}
