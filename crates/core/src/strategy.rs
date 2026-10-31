//! `Strategy` trait'i ve `Ctx`.
//!
//! Strateji yalnız `Ctx`'ten okur (kural 4). `Ctx`'teki geçmiş, motorun **işlemiş olduğu**
//! olaylardan oluşur; henüz gelmemiş veri tip düzeyinde erişilemez. Strateji kodu backtest mi
//! canlı mı olduğunu bilmez (kural 3).

use crate::instrument::InstrumentId;
use crate::intent::{Intent, Side, StrategyId};
use crate::market::{Bar, FeedStatus, MarketEvent, MarketPayload, Quote, Trade};
use crate::units::{Money, Price, UnixNanos};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionView {
    pub side: Side,
    pub qty: i32,
    pub entry: Price,
    pub stop: Price,
    pub opened: UnixNanos,
}

/// Hesap görünümü (salt okunur).
pub trait AccountView {
    fn position(&self, strategy: &StrategyId, inst: InstrumentId) -> Option<PositionView>;
    fn has_pending_entry(&self, strategy: &StrategyId, inst: InstrumentId) -> bool;
    fn equity(&self) -> Money;
}

/// Seans görünümü (ders §9.6: seans/hafta sonuna kalan süre).
pub trait SessionView {
    fn is_open(&self, inst: InstrumentId, ts: UnixNanos) -> bool;
    /// Bir sonraki kapanışa (günlük ara, tatil, hafta sonu) kalan nanosaniye.
    fn time_to_session_close(&self, inst: InstrumentId, ts: UnixNanos) -> Option<u64>;
    fn time_to_week_close(&self, inst: InstrumentId, ts: UnixNanos) -> Option<u64>;
}

#[derive(Debug, Clone, Default)]
pub struct InstrumentHistory {
    bars: Vec<Bar>,
    last_trade: Option<(UnixNanos, Trade)>,
    last_quote: Option<(UnixNanos, Quote)>,
    last_price: Option<Price>,
    feed_down: bool,
}

impl InstrumentHistory {
    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }
    pub fn last_trade(&self) -> Option<(UnixNanos, Trade)> {
        self.last_trade
    }
    pub fn last_quote(&self) -> Option<(UnixNanos, Quote)> {
        self.last_quote
    }
    pub fn last_price(&self) -> Option<Price> {
        self.last_price
    }
    pub fn feed_down(&self) -> bool {
        self.feed_down
    }
}

/// İşlenmiş piyasa olaylarının geçmişi.
#[derive(Debug, Clone, Default)]
pub struct MarketHistory {
    per: BTreeMap<InstrumentId, InstrumentHistory>,
    /// Enstrüman başına en az tutulacak bar (0 = sınırsız); tampon en fazla 2× bu kadar büyür.
    max_bars: usize,
    now: UnixNanos,
}

impl MarketHistory {
    pub fn new(max_bars: usize) -> Self {
        MarketHistory {
            per: BTreeMap::new(),
            max_bars,
            now: UnixNanos::ZERO,
        }
    }

    pub fn now(&self) -> UnixNanos {
        self.now
    }

    pub fn push(&mut self, ev: &MarketEvent) {
        if ev.ts > self.now {
            self.now = ev.ts;
        }
        let h = self.per.entry(ev.instrument).or_default();
        match &ev.payload {
            MarketPayload::Bar(b) => {
                // Amortize kırpma: tampon 2×max'a ulaşınca en eski max bar atılır (O(1) ortalama).
                if self.max_bars > 0 && h.bars.len() >= 2 * self.max_bars {
                    h.bars.drain(..self.max_bars);
                }
                h.bars.push(*b);
                h.last_price = Some(b.close);
            }
            MarketPayload::Trade(t) => {
                h.last_trade = Some((ev.ts, *t));
                h.last_price = Some(t.price);
            }
            MarketPayload::Quote(q) => h.last_quote = Some((ev.ts, *q)),
            // Kesinti fiyat değildir: yalnız durum bayrağı (ders §9.2).
            MarketPayload::Feed(FeedStatus::Down { .. }) => h.feed_down = true,
            MarketPayload::Feed(FeedStatus::Up) => h.feed_down = false,
        }
    }

    pub fn get(&self, inst: InstrumentId) -> Option<&InstrumentHistory> {
        self.per.get(&inst)
    }
}

pub struct Ctx<'a> {
    pub strategy: &'a StrategyId,
    history: &'a MarketHistory,
    account: &'a dyn AccountView,
    sessions: &'a dyn SessionView,
}

impl<'a> Ctx<'a> {
    pub fn new(
        strategy: &'a StrategyId,
        history: &'a MarketHistory,
        account: &'a dyn AccountView,
        sessions: &'a dyn SessionView,
    ) -> Self {
        Ctx {
            strategy,
            history,
            account,
            sessions,
        }
    }

    pub fn now(&self) -> UnixNanos {
        self.history.now()
    }
    pub fn bars(&self, inst: InstrumentId) -> &[Bar] {
        self.history.get(inst).map_or(&[], InstrumentHistory::bars)
    }
    pub fn last_price(&self, inst: InstrumentId) -> Option<Price> {
        self.history
            .get(inst)
            .and_then(InstrumentHistory::last_price)
    }
    pub fn last_trade(&self, inst: InstrumentId) -> Option<(UnixNanos, Trade)> {
        self.history
            .get(inst)
            .and_then(InstrumentHistory::last_trade)
    }
    pub fn feed_down(&self, inst: InstrumentId) -> bool {
        self.history
            .get(inst)
            .is_some_and(InstrumentHistory::feed_down)
    }
    /// Bu stratejinin pozisyonu.
    pub fn position(&self, inst: InstrumentId) -> Option<PositionView> {
        self.account.position(self.strategy, inst)
    }
    pub fn has_pending_entry(&self, inst: InstrumentId) -> bool {
        self.account.has_pending_entry(self.strategy, inst)
    }
    pub fn is_flat(&self, inst: InstrumentId) -> bool {
        self.position(inst).is_none() && !self.has_pending_entry(inst)
    }
    pub fn equity(&self) -> Money {
        self.account.equity()
    }
    pub fn session_open(&self, inst: InstrumentId) -> bool {
        self.sessions.is_open(inst, self.now())
    }
    pub fn time_to_session_close(&self, inst: InstrumentId) -> Option<u64> {
        self.sessions.time_to_session_close(inst, self.now())
    }
    pub fn time_to_week_close(&self, inst: InstrumentId) -> Option<u64> {
        self.sessions.time_to_week_close(inst, self.now())
    }
}

pub trait Strategy {
    fn id(&self) -> &StrategyId;
    fn on_event(&mut self, ctx: &Ctx<'_>, ev: &MarketEvent) -> Vec<Intent>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_caps_bars_and_ignores_feed_for_price() {
        let mut h = MarketHistory::new(1);
        let i = InstrumentId(0);
        for k in 0..3u64 {
            h.push(&MarketEvent::bar(
                i,
                Bar {
                    open_ts: UnixNanos(k * 60),
                    close_ts: UnixNanos(k * 60 + 60),
                    open: Price(10),
                    high: Price(12),
                    low: Price(9),
                    close: Price(11 + k as i64),
                    volume: 1,
                    buy_volume: 0,
                    trades: 1,
                },
            ));
        }
        let ih = h.get(i).unwrap();
        assert_eq!(ih.bars().len(), 2);
        assert_eq!(ih.last_price(), Some(Price(13)));
        h.push(&MarketEvent {
            ts: UnixNanos(500),
            instrument: i,
            payload: MarketPayload::Feed(FeedStatus::Down { reason: "x".into() }),
        });
        let ih = h.get(i).unwrap();
        assert!(ih.feed_down());
        assert_eq!(ih.last_price(), Some(Price(13)));
        assert_eq!(h.now(), UnixNanos(500));
    }
}
