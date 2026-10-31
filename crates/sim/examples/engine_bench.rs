#![allow(clippy::unwrap_used)]
//! Motor olay başı maliyeti: `cargo run --release -p atlas-sim --example engine_bench`
use atlas_core::{Ctx, InstrumentId, Intent, MarketEvent, Price, Strategy, StrategyId, UnixNanos};
use atlas_sim::synthetic::ar_bars;
use atlas_sim::validation::{run_sim, validation_env};
use atlas_sim::{BarFill, SimConfig};
use std::time::Instant;

struct Noop(StrategyId);
impl Strategy for Noop {
    fn id(&self) -> &StrategyId {
        &self.0
    }
    fn on_event(&mut self, ctx: &Ctx<'_>, _: &MarketEvent) -> Vec<Intent> {
        let _ = ctx.is_flat(InstrumentId(0));
        Vec::new()
    }
}

struct Trader(StrategyId, u64, u64);
impl Strategy for Trader {
    fn id(&self) -> &StrategyId {
        &self.0
    }
    fn on_event(&mut self, ctx: &Ctx<'_>, _: &MarketEvent) -> Vec<Intent> {
        self.1 += 1;
        let i = InstrumentId(0);
        if !self.1.is_multiple_of(32) || !ctx.is_flat(i) {
            return Vec::new();
        }
        let p = ctx.last_price(i).unwrap();
        vec![Intent {
            strategy: self.0.clone(),
            instrument: i,
            ts: ctx.now(),
            action: atlas_core::Action::Open {
                side: atlas_core::Side::Buy,
                qty: 1,
                entry: atlas_core::EntryType::Market,
                stop_loss: p - 500,
                take_profit: Some(p + 500),
                time_exit: Some(ctx.now().plus(self.2 * 60_000_000_000)),
                entry_expiry: None,
            },
        }]
    }
}

fn main() {
    let env = validation_env().unwrap();
    let bars = ar_bars(
        1,
        921_000,
        UnixNanos::from_secs(1_700_000_000),
        0.0,
        6.0,
        Price(100_000),
    );
    let evs: Vec<MarketEvent> = bars
        .iter()
        .map(|b| MarketEvent::bar(InstrumentId(0), *b))
        .collect();
    for (name, kind) in [
        ("strateji yok", 0u64),
        ("noop strateji", 1),
        ("her 32 barda islem, 16 bar tut", 16),
        ("her 32 barda islem, 1 bar tut", 1),
    ] {
        let cfg = SimConfig {
            max_bars_history: 512,
            max_rejection_events: 0,
            record_position_events: false,
            ..SimConfig::default()
        };
        let mut s: Vec<Box<dyn Strategy>> = match kind {
            0 if name.starts_with("strateji") => vec![],
            1 if name.starts_with("noop") => vec![Box::new(Noop(StrategyId::new("n")))],
            h => vec![Box::new(Trader(StrategyId::new("t"), 0, h))],
        };
        let t = Instant::now();
        let r = run_sim(
            &env,
            std::sync::Arc::new(BarFill::default()),
            cfg,
            evs.clone(),
            &mut s,
        )
        .unwrap();
        println!(
            "{name}: {} olay, {:.0} ms",
            r.market_events,
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
}
