#![allow(clippy::unwrap_used)]
//! Gauntlet'in iki yönlü testi, sentetik veriyle (CI'da her sürümde koşar):
//! 1. Sahte strateji testi: edge'i olmayan veride rastgele genomların ≤ %1'i geçer.
//! 2. Güç testi: bilinçli momentum yerleştirilmiş veride trend genomu gauntlet'i geçer.

use atlas_core::{InstrumentTable, SessionSpec, UnixNanos};
use atlas_features::FeatureStore;
use atlas_gauntlet::eval::{Evaluator, Market, always_calendar, research_env};
use atlas_gauntlet::holdout::split;
use atlas_gauntlet::{Context, GauntletConfig, fake};
use atlas_genome::Genome;
use atlas_registry::Registry;
use atlas_risk::PropRules;
use atlas_sim::synthetic::ar_bars;
use std::sync::Arc;

const INSTR: &str = r#"
[[instrument]]
symbol = "SYN"
venue = "binance"
kind = "future"
tick_size = "1"
tick_value_usd = "1"
session = "always"
[instrument.costs]
commission_per_contract_usd = "0.5"
slippage_ticks = "1"
"#;

const PROP: &str = r#"
firm = "GENEL (test)"
account_label = "t"
starting_balance_usd = "100000"
profit_target_usd = "10000"
drawdown_type = "static"
drawdown_amount_usd = "10000"
drawdown_lock_level_usd = "none"
daily_loss_limit_usd = "5000"
daily_loss_reference = "day_start_balance"
day_reset_tz = "UTC"
day_reset_time = "00:00"
consistency_ratio = "none"
max_contracts = "5"
rules_tz = "UTC"
no_new_entries_after = "none"
flat_by = "none"
weekend_holding_allowed = true
news_blackout_minutes_before = "none"
news_blackout_minutes_after = "none"
algo_trading_allowed = true
"#;

/// 1 barlık momentum yakalayıcı: son bar yükseldiyse bir sonraki barı long (düştüyse short) tut.
const MOMENTUM: &str = r#"
name = "momentum_test"
instrument = "SYN"
[entry]
long = { gt = ["close", "open"] }
short = { lt = ["close", "open"] }
[exit]
atr = 20
stop_atr = 4.0
time_bars = 1
[sizing]
risk_usd = 1000
"#;

struct Setup {
    table: InstrumentTable,
    is_bars: Vec<atlas_core::Bar>,
    vault: atlas_gauntlet::holdout::HoldoutVault,
}

fn setup(phi: f64, sigma: f64, seed: u64, n: usize, budget: u64) -> Setup {
    let table = InstrumentTable::from_toml_str(INSTR).unwrap();
    let bars = ar_bars(
        seed,
        n,
        UnixNanos::from_secs(1_700_000_000),
        phi,
        sigma,
        atlas_core::Price(100_000),
    );
    let (is_bars, vault) = split("SYN", bars, 20, budget);
    Setup {
        table,
        is_bars,
        vault,
    }
}

fn market(s: &Setup, ver: &str) -> Market {
    Market {
        dataset: "SYN".into(),
        data_version: ver.into(),
        instrument: s.table.get("SYN").unwrap().clone(),
        session: SessionSpec::Always,
        bars: Arc::new(s.is_bars.clone()),
    }
}

#[test]
fn fake_strategies_pass_at_most_one_percent() {
    let s = setup(0.0, 6.0, 11, 150_000, 1_000);
    let store = FeatureStore::new();
    let ev = Evaluator::new(research_env(&s.table, &always_calendar()).unwrap(), &store);
    let m = market(&s, "rw-11");
    let prop = PropRules::from_toml_str(PROP).unwrap();
    let cfg = GauntletConfig {
        mc_paths: 300,
        ..GauntletConfig::default()
    };
    let ctx = Context {
        eval: &ev,
        market: &m,
        vault: &s.vault,
        prop: &prop,
        cfg: &cfg,
        seed: 5,
        cost: atlas_gauntlet::eval::Cost::BASE,
    };
    let mut reg = Registry::open_in_memory().unwrap();
    let genomes = fake::candidates(300, 42, "SYN", 1000);
    let out = fake::run(&ctx, &genomes, &mut reg, UnixNanos(1)).unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&out).unwrap());
    assert!(out.green, "{out:?}");
    assert_eq!(reg.trial_count().unwrap(), 300);
    // Her başarısız aday için ders yazıldı.
    assert_eq!(
        reg.summary().unwrap().lessons as usize,
        300 - out.passed_final
    );
}

#[test]
fn planted_edge_passes_and_noise_version_fails() {
    let prop = PropRules::from_toml_str(PROP).unwrap();
    let cfg = GauntletConfig {
        mc_paths: 500,
        ..GauntletConfig::default()
    };
    let g = Genome::from_toml_str(MOMENTUM).unwrap();
    let mut results = Vec::new();
    for (phi, ver) in [(0.3, "ar-0.3"), (0.0, "ar-0.0")] {
        let s = setup(phi, 30.0, 3, 150_000, 10);
        let store = FeatureStore::new();
        let ev = Evaluator::new(research_env(&s.table, &always_calendar()).unwrap(), &store);
        let m = market(&s, ver);
        let ctx = Context {
            eval: &ev,
            market: &m,
            vault: &s.vault,
            prop: &prop,
            cfg: &cfg,
            seed: 9,
            cost: atlas_gauntlet::eval::Cost::BASE,
        };
        let mut reg = Registry::open_in_memory().unwrap();
        let (rep, _) = atlas_gauntlet::run(&ctx, &g, &mut reg, "test", None, UnixNanos(1)).unwrap();
        eprintln!("{}", rep.to_markdown());
        results.push(rep);
    }
    assert!(
        results[0].passed,
        "yerleştirilmiş edge geçmeli: {:?}",
        results[0].failed_gate
    );
    assert!(!results[1].passed, "edge'siz veri geçmemeli");
}
