//! Genom değerlendirici: genomu simülasyon motorunda (BarFill) çalıştırıp işlem listesi üretir.
//! Feature sütunları `FeatureStore` önbelleğinden gelir; aynı veri üzerinde binlerce genom ucuzdur.

use atlas_core::{Bar, Instrument, InstrumentTable, MarketEvent, SessionSpec, Strategy, UnixNanos};
use atlas_features::FeatureStore;
use atlas_genome::Genome;
use atlas_genome::strategy::GenomeStrategy;
use atlas_risk::{PropRules, RiskEnv, RiskGate, RiskPolicy};
use atlas_sim::validation::{VALIDATION_CALENDAR, VALIDATION_POLICY, VALIDATION_PROP};
use atlas_sim::{BarFill, Engine, SimConfig, TradeRecord};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    #[error("genom: {0}")]
    Genome(#[from] atlas_genome::GenomeError),
    #[error("sim: {0}")]
    Sim(#[from] atlas_sim::SimError),
    #[error("ortam: {0}")]
    Env(String),
}

/// Değerlendirilecek veri: tek enstrüman, barlar ve versiyonu.
#[derive(Clone)]
pub struct Market {
    pub dataset: String,
    pub data_version: String,
    pub instrument: Instrument,
    pub session: SessionSpec,
    pub bars: Arc<Vec<Bar>>,
}

impl Market {
    pub fn span(&self) -> (UnixNanos, UnixNanos) {
        (
            self.bars.first().map_or(UnixNanos::ZERO, |b| b.open_ts),
            self.bars.last().map_or(UnixNanos::ZERO, |b| b.close_ts),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    pub slippage_mult_milli: i64,
    pub fee_mult_milli: i64,
}

impl Cost {
    pub const BASE: Cost = Cost {
        slippage_mult_milli: 1000,
        fee_mult_milli: 1000,
    };
    pub const ZERO: Cost = Cost {
        slippage_mult_milli: 0,
        fee_mult_milli: 0,
    };
}

/// Araştırma koşuları için hesap: prop kuralları burada uygulanmaz (veriyi kesmesin diye);
/// prop kuralları kapı 9'da işlem listesine Monte Carlo ile uygulanır.
pub fn research_env(
    instruments: &InstrumentTable,
    calendar: &atlas_core::Calendar,
) -> Result<RiskEnv, EvalError> {
    Ok(RiskEnv {
        rules: PropRules::from_toml_str(VALIDATION_PROP)
            .map_err(|e| EvalError::Env(e.to_string()))?,
        policy: RiskPolicy::from_toml_str(VALIDATION_POLICY)
            .map_err(|e| EvalError::Env(e.to_string()))?,
        instruments: instruments.clone(),
        calendar: calendar.clone(),
    })
}

/// Yalnız `always` seansı olan takvim (sentetik veri, testler).
pub fn always_calendar() -> atlas_core::Calendar {
    atlas_core::Calendar::from_toml_str(VALIDATION_CALENDAR).unwrap_or_default()
}

pub struct Evaluator<'a> {
    pub gate: RiskGate,
    pub store: &'a FeatureStore,
}

impl<'a> Evaluator<'a> {
    pub fn new(env: RiskEnv, store: &'a FeatureStore) -> Self {
        Evaluator {
            gate: RiskGate::new(env),
            store,
        }
    }

    /// Genomu çalıştırır. `from`: yalnız bu andan sonra girilen işlemler döner (holdout).
    pub fn run(
        &self,
        g: &Genome,
        m: &Market,
        cost: Cost,
        from: Option<UnixNanos>,
    ) -> Result<Vec<TradeRecord>, EvalError> {
        let specs = g.feature_specs()?;
        let shared = self
            .store
            .columns(&m.data_version, &m.session, &m.bars, &specs);
        let strat =
            GenomeStrategy::with_columns(g, m.instrument.id, m.instrument.tick_value, &shared)?;
        let cfg = SimConfig {
            slippage_mult_milli: cost.slippage_mult_milli,
            fee_mult_milli: cost.fee_mult_milli,
            max_rejection_events: 0,
            max_bars_history: 512,
            record_position_events: false,
            ..SimConfig::default()
        };
        let start = m.span().0;
        let fill = BarFill::default();
        let engine = Engine::new(&self.gate, &fill, cfg, start);
        let inst = m.instrument.id;
        let mut strats: Vec<Box<dyn Strategy>> = vec![Box::new(strat)];
        let res = engine.run(
            m.bars.iter().map(|b| MarketEvent::bar(inst, *b)),
            &mut strats,
        )?;
        let mut t = res.trades;
        if let Some(f) = from {
            t.retain(|x| x.entry_ts >= f);
        }
        Ok(t)
    }
}

pub fn nets(t: &[TradeRecord]) -> Vec<f64> {
    t.iter().map(|x| x.net.as_usd_f64()).collect()
}
