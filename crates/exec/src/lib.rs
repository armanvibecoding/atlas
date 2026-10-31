//! Yürütme (§3 `exec`, §4.12): canlı veri akışları ve strateji koşucuları.
//!
//! - `LiveFeed`: `BinanceKlineFeed` (REST yoklama; her yoklamada son görülen bardan bu yana TÜM kapanmış
//!   barları çeker → boşluk otomatik telafi edilir, ders §9.1) ve `ReplayFeed` (depodaki barlarla
//!   hızlandırılmış koşu; aynı daemon kodu sanal saatle çalışır).
//! - Kesinti fiyat olayı değildir (ders §9.2): yoklama başarısızsa `FeedStatus::Down` olayı üretilir,
//!   fiyat değişmez; düzelince `Up`.
//! - `Runner`: tek stratejinin canlı (gölge/paper) koşucusu. `Engine` + strateji aynı kod yolu (kural 3).
//!   Yeniden başlatmada depodaki barlar yeniden oynatılarak durum deterministik olarak yeniden kurulur.
//!
//! Executor ayrımı: gölge ve paper aynı simüle borsayı (BarFill) kullanır; fark terfi kriteri ve
//! kayıttır. Gerçek borsa executor'ı (Rithmic/Tradovate/MT5) M5'te bu arayüzün arkasına gelir.

pub mod manual;

use atlas_core::units::NANOS_PER_MIN;
use atlas_core::{
    Bar, Ctx, Event, EventKind, FeedStatus, InstrumentId, Intent, MarketEvent, MarketPayload,
    Strategy, StrategyId, UnixNanos,
};
use atlas_portfolio::{DriftMonitor, DriftUpdate, Expected};
use atlas_registry::ladder::Stage;
use atlas_risk::RiskGate;
use atlas_sim::{BarFill, Engine, SimConfig, TradeRecord};
use std::sync::Arc;

pub trait LiveFeed {
    fn symbol(&self) -> &str;
    fn instrument(&self) -> InstrumentId;
    /// `now` anına kadar kapanmış, daha önce verilmemiş barlar (+ durum olayları), zaman sırasında.
    fn poll(&mut self, now: UnixNanos) -> Vec<MarketEvent>;
    /// Kalan bar sayısı (yalnız sonlu akışlar; canlı akış `None`).
    fn remaining(&self) -> Option<usize> {
        None
    }
}

/// Binance USDⓈ-M 1m REST yoklaması.
pub struct BinanceKlineFeed {
    api: atlas_data::binance::BinanceFutures,
    inst: atlas_core::Instrument,
    last_close: UnixNanos,
    down_since: Option<UnixNanos>,
    reported_down: bool,
    pub max_quiet: u64,
}

impl BinanceKlineFeed {
    pub fn new(
        api: atlas_data::binance::BinanceFutures,
        inst: atlas_core::Instrument,
        last_close: UnixNanos,
    ) -> Self {
        BinanceKlineFeed {
            api,
            inst,
            last_close,
            down_since: None,
            reported_down: false,
            max_quiet: 3 * NANOS_PER_MIN,
        }
    }
}

impl LiveFeed for BinanceKlineFeed {
    fn symbol(&self) -> &str {
        &self.inst.symbol
    }
    fn instrument(&self) -> InstrumentId {
        self.inst.id
    }
    fn poll(&mut self, now: UnixNanos) -> Vec<MarketEvent> {
        let end = UnixNanos(now.0 - now.0 % NANOS_PER_MIN);
        if end <= self.last_close {
            return Vec::new();
        }
        // Son kapanıştan itibaren (boşluk telafisi dahil) tüm kapanmış barlar.
        match self.api.klines_1m(&self.inst, self.last_close, end) {
            Ok(bars) => {
                let mut out = Vec::new();
                if self.reported_down {
                    out.push(MarketEvent {
                        ts: now,
                        instrument: self.inst.id,
                        payload: MarketPayload::Feed(FeedStatus::Up),
                    });
                }
                self.down_since = None;
                self.reported_down = false;
                let from = self.last_close;
                for b in bars
                    .into_iter()
                    .filter(|b| b.open_ts >= from && b.close_ts <= end)
                {
                    self.last_close = b.close_ts;
                    out.push(MarketEvent::bar(self.inst.id, b));
                }
                out.sort_by_key(|e| e.ts);
                out
            }
            Err(e) => {
                let since = *self.down_since.get_or_insert(now);
                if !self.reported_down
                    && now.since(since) >= self.max_quiet.min(now.since(self.last_close))
                {
                    self.reported_down = true;
                    return vec![MarketEvent {
                        ts: now,
                        instrument: self.inst.id,
                        payload: MarketPayload::Feed(FeedStatus::Down {
                            reason: e.to_string().chars().take(300).collect(),
                        }),
                    }];
                }
                Vec::new()
            }
        }
    }
}

/// Depodaki barlarla hızlandırılmış/yeniden oynatma akışı.
pub struct ReplayFeed {
    symbol: String,
    inst: InstrumentId,
    bars: Vec<Bar>,
    pos: usize,
}

impl ReplayFeed {
    pub fn new(symbol: &str, inst: InstrumentId, bars: Vec<Bar>) -> Self {
        ReplayFeed {
            symbol: symbol.into(),
            inst,
            bars,
            pos: 0,
        }
    }
    pub fn left(&self) -> usize {
        self.bars.len() - self.pos
    }
}

impl LiveFeed for ReplayFeed {
    fn symbol(&self) -> &str {
        &self.symbol
    }
    fn instrument(&self) -> InstrumentId {
        self.inst
    }
    fn remaining(&self) -> Option<usize> {
        Some(self.left())
    }
    fn poll(&mut self, now: UnixNanos) -> Vec<MarketEvent> {
        let mut out = Vec::new();
        while let Some(b) = self.bars.get(self.pos) {
            if b.close_ts > now {
                break;
            }
            out.push(MarketEvent::bar(self.inst, *b));
            self.pos += 1;
        }
        out
    }
}

/// Isınma süresince stratejinin niyetlerini bastırır (feature'lar ısınır, işlem açılmaz).
struct Warmup<S> {
    inner: S,
    live_from: UnixNanos,
}

impl<S: Strategy> Strategy for Warmup<S> {
    fn id(&self) -> &StrategyId {
        self.inner.id()
    }
    fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent> {
        let v = self.inner.on_event(ctx, ev);
        if ev.ts < self.live_from {
            Vec::new()
        } else {
            v
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StepReport {
    pub events: Vec<Event>,
    pub new_trades: Vec<TradeRecord>,
    pub drift: Vec<DriftUpdate>,
}

/// Tek stratejinin canlı koşucusu.
pub struct Runner {
    pub genome_hash: String,
    pub name: String,
    pub stage: Stage,
    pub symbol: String,
    pub live_from: UnixNanos,
    pub risk_usd: f64,
    engine: Engine,
    strategies: Vec<Box<dyn Strategy>>,
    seen_trades: usize,
    pub drift: DriftMonitor,
}

impl Runner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        genome: &atlas_genome::Genome,
        inst: &atlas_core::Instrument,
        session: atlas_core::SessionSpec,
        gate: Arc<RiskGate>,
        stage: Stage,
        live_from: UnixNanos,
        start: UnixNanos,
        expected: Expected,
    ) -> Result<Runner, atlas_genome::GenomeError> {
        let strat = atlas_genome::strategy::GenomeStrategy::live(
            genome,
            inst.id,
            inst.tick_value,
            session,
        )?;
        let cfg = SimConfig {
            max_bars_history: 512,
            ..SimConfig::default()
        };
        Ok(Runner {
            genome_hash: genome.hash(),
            name: genome.name.clone(),
            stage,
            symbol: inst.symbol.clone(),
            live_from,
            risk_usd: f64::from(genome.sizing.risk_usd).max(1.0),
            engine: Engine::new(gate, Arc::new(BarFill::default()), cfg, start),
            strategies: vec![Box::new(Warmup {
                inner: strat,
                live_from,
            })],
            seen_trades: 0,
            drift: DriftMonitor::new(expected),
        })
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    pub fn trades(&self) -> &[TradeRecord] {
        self.engine.trades()
    }

    /// Bir piyasa olayı işler; yeni kapanan işlemler sapma izleyicisine (R biriminde) girer.
    pub fn step(&mut self, ev: &MarketEvent) -> Result<StepReport, atlas_sim::SimError> {
        if ev.ts < self.engine.last_ts() {
            return Ok(StepReport {
                events: Vec::new(),
                new_trades: Vec::new(),
                drift: Vec::new(),
            });
        }
        self.engine.step(ev, &mut self.strategies)?;
        let mut events = self.engine.drain_events();
        for e in &mut events {
            if let serde_json::Value::Object(m) = &mut e.payload {
                m.insert("stage".into(), serde_json::json!(self.stage));
                m.insert("strategy_name".into(), serde_json::json!(self.name));
            }
        }
        let all = self.engine.trades();
        let new: Vec<TradeRecord> = all[self.seen_trades..].to_vec();
        self.seen_trades = all.len();
        let drift = new
            .iter()
            .filter(|t| t.entry_ts >= self.live_from)
            .map(|t| self.drift.update(t.net.as_usd_f64() / self.risk_usd))
            .collect();
        Ok(StepReport {
            events,
            new_trades: new,
            drift,
        })
    }

    /// Gölge aşamasında işlemler emir sayılmaz: olaylar `shadow_signal` olarak etiketlenir.
    pub fn shadow_events(&self, trades: &[TradeRecord], now: UnixNanos) -> Vec<Event> {
        trades
            .iter()
            .map(|t| {
                Event::new(
                    now,
                    EventKind::ShadowSignal,
                    serde_json::json!({"strategy": self.name, "hash": &self.genome_hash[..12], "side": t.side,
                        "entry": t.entry_price, "exit": t.exit_price, "net_r": t.net.as_usd_f64() / self.risk_usd,
                        "reason": t.exit_reason}),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::{Price, SessionSpec};

    #[test]
    fn replay_feed_emits_in_order_and_only_closed() {
        let bars = atlas_sim::synthetic::ar_bars(1, 10, UnixNanos(0), 0.0, 5.0, Price(1000));
        let mut f = ReplayFeed::new("SYN", InstrumentId(0), bars.clone());
        assert!(f.poll(UnixNanos(NANOS_PER_MIN - 1)).is_empty());
        assert_eq!(f.poll(UnixNanos(3 * NANOS_PER_MIN)).len(), 3);
        assert_eq!(f.poll(UnixNanos(u64::MAX)).len(), 7);
        assert_eq!(f.left(), 0);
    }

    #[test]
    fn runner_warms_up_then_trades_and_feeds_drift() {
        let env = atlas_sim::validation::validation_env().unwrap();
        let inst = env.instruments.by_id(InstrumentId(0)).unwrap().clone();
        let g = atlas_genome::Genome::from_toml_str(
            r#"
name = "m"
instrument = "SYN"
[entry]
long = { gt = ["close", "open"] }
short = { lt = ["close", "open"] }
[exit]
atr = 10
stop_atr = 3.0
time_bars = 1
[sizing]
risk_usd = 100000
"#,
        )
        .unwrap();
        let bars = atlas_sim::synthetic::ar_bars(2, 3000, UnixNanos(0), 0.3, 30.0, Price(100_000));
        let live_from = bars[1000].open_ts;
        let mut r = Runner::new(
            &g,
            &inst,
            SessionSpec::Always,
            Arc::new(RiskGate::new(env)),
            Stage::Shadow,
            live_from,
            UnixNanos(0),
            Expected {
                mean_r: 0.1,
                sd_r: 1.0,
            },
        )
        .unwrap();
        let mut early = 0;
        let mut drift_n = 0;
        for b in &bars {
            let rep = r.step(&MarketEvent::bar(InstrumentId(0), *b)).unwrap();
            if b.close_ts <= live_from {
                early += rep.new_trades.len();
            }
            drift_n += rep.drift.len();
        }
        assert_eq!(early, 0, "ısınmada işlem açılmamalı");
        assert!(drift_n > 100, "{drift_n}");
        assert_eq!(drift_n, r.drift.results_r.len());
    }
}
