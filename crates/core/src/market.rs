//! Piyasa olayları. Backtest ve canlı aynı tipi üretir (kural 3).
//!
//! `MarketEvent::ts` olayın **kullanılabilir olduğu** andır: 1 dakikalık bar için kapanış anı,
//! açılış değil. Bu, lookahead'i tip düzeyinde engeller (kural 4).

use crate::instrument::InstrumentId;
use crate::intent::Side;
use crate::units::{Price, UnixNanos};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bar {
    pub open_ts: UnixNanos,
    /// Barın kapandığı an = olayın kullanılabilir olduğu an.
    pub close_ts: UnixNanos,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    /// Lot cinsinden hacim.
    pub volume: u64,
    pub trades: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trade {
    pub price: Price,
    /// Lot cinsinden miktar.
    pub size: u64,
    /// Agresör tarafı (bilinmiyorsa `None`).
    pub aggressor: Option<Side>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub bid: Price,
    pub ask: Price,
    pub bid_size: u64,
    pub ask_size: u64,
}

/// Veri akışı durumu. **Kesinti asla fiyat olayı değildir** (ders §9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedStatus {
    Down { reason: String },
    Up,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketPayload {
    Bar(Bar),
    Trade(Trade),
    Quote(Quote),
    Feed(FeedStatus),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketEvent {
    pub ts: UnixNanos,
    pub instrument: InstrumentId,
    pub payload: MarketPayload,
}

impl MarketEvent {
    pub fn bar(instrument: InstrumentId, bar: Bar) -> Self {
        MarketEvent {
            ts: bar.close_ts,
            instrument,
            payload: MarketPayload::Bar(bar),
        }
    }

    pub fn trade(instrument: InstrumentId, ts: UnixNanos, trade: Trade) -> Self {
        MarketEvent {
            ts,
            instrument,
            payload: MarketPayload::Trade(trade),
        }
    }

    /// Bu olaydan elde edilen son işlem fiyatı (bar kapanışı, trade fiyatı). Kesinti için `None`.
    pub fn last_price(&self) -> Option<Price> {
        match &self.payload {
            MarketPayload::Bar(b) => Some(b.close),
            MarketPayload::Trade(t) => Some(t.price),
            MarketPayload::Quote(_) | MarketPayload::Feed(_) => None,
        }
    }
}
