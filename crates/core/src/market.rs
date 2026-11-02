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
    /// Alıcı-agresör (taker buy) hacmi, lot. Kaynak vermiyorsa 0 ve `volume`'dan bağımsızdır.
    pub buy_volume: u64,
    pub trades: u32,
}

impl Bar {
    /// Bar deltası: alıcı-agresör − satıcı-agresör hacmi (lot).
    pub fn delta(&self) -> i64 {
        let b = i64::try_from(self.buy_volume).unwrap_or(i64::MAX);
        let v = i64::try_from(self.volume).unwrap_or(i64::MAX);
        2 * b - v
    }
}

/// 1 dakikalık barları `minutes`'lık UTC hizalı kovalara artımlı toplar. Backtest ([`aggregate_bars`])
/// ve canlı koşucu aynı toplayıcıyı kullanır (kural 3). Toplu barın `close_ts`'i kovadaki son alt
/// barınki: olay ancak o alt bar kapanınca kullanılabilir (kural 4). Boş kova bar üretmez.
#[derive(Debug, Clone)]
pub struct BarAggregator {
    span: u64,
    cur: Option<(u64, Bar)>,
}

impl BarAggregator {
    pub fn new(minutes: u64) -> Self {
        BarAggregator {
            span: minutes.max(1) * crate::units::NANOS_PER_MIN,
            cur: None,
        }
    }

    /// Bir alt bar ekler; tamamlanan kovalar döner (0–2: eksik dakikalı önceki kova + bu bar
    /// kovayı kapatıyorsa kendi kovası). Kova, sonraki dakikası yeni kovaya düşen alt barla kapanır.
    pub fn push(&mut self, b: &Bar) -> Vec<Bar> {
        let k = b.open_ts.0 / self.span;
        let mut out = Vec::new();
        match &mut self.cur {
            Some((ck, l)) if *ck == k => {
                l.close_ts = b.close_ts;
                l.high = l.high.max(b.high);
                l.low = l.low.min(b.low);
                l.close = b.close;
                l.volume = l.volume.saturating_add(b.volume);
                l.buy_volume = l.buy_volume.saturating_add(b.buy_volume);
                l.trades = l.trades.saturating_add(b.trades);
            }
            _ => {
                if let Some((_, prev)) = self.cur.take() {
                    out.push(prev);
                }
                self.cur = Some((
                    k,
                    Bar {
                        open_ts: UnixNanos(k * self.span),
                        ..*b
                    },
                ));
            }
        }
        if (b.open_ts.0 + crate::units::NANOS_PER_MIN) / self.span != k
            && let Some((_, done)) = self.cur.take()
        {
            out.push(done);
        }
        out
    }

    /// Yarım kalan son kova (yalnız toplu tarihsel veride).
    pub fn flush(&mut self) -> Option<Bar> {
        self.cur.take().map(|(_, b)| b)
    }
}

/// Tarihsel barları toplar; `minutes <= 1` ise girdi aynen döner.
pub fn aggregate_bars(bars: &[Bar], minutes: u64) -> Vec<Bar> {
    if minutes <= 1 {
        return bars.to_vec();
    }
    let mut agg = BarAggregator::new(minutes);
    let mut out: Vec<Bar> = Vec::with_capacity(bars.len() / minutes as usize + 1);
    for b in bars {
        out.extend(agg.push(b));
    }
    out.extend(agg.flush());
    out
}

#[cfg(test)]
mod aggregate_tests {
    use super::*;

    fn bar(min: u64, o: i64, h: i64, l: i64, c: i64) -> Bar {
        let open_ts = UnixNanos(min * crate::units::NANOS_PER_MIN);
        Bar {
            open_ts,
            close_ts: UnixNanos(open_ts.0 + crate::units::NANOS_PER_MIN - 1),
            open: Price(o),
            high: Price(h),
            low: Price(l),
            close: Price(c),
            volume: 2,
            buy_volume: 1,
            trades: 1,
        }
    }

    #[test]
    fn aggregates_aligned_buckets_without_lookahead() {
        // 14:59, 15:00..15:14 (eksik dakika dahil), 15:15
        let mut bars = vec![bar(899, 10, 11, 9, 10)];
        for m in 900..915 {
            if m != 905 {
                bars.push(bar(
                    m,
                    100 + (m as i64 - 900),
                    200,
                    50 + (m as i64 - 900),
                    101,
                ));
            }
        }
        bars.push(bar(915, 1, 2, 0, 1));
        let a = aggregate_bars(&bars, 15);
        assert_eq!(a.len(), 3);
        let b = a[1];
        assert_eq!(b.open_ts, UnixNanos(900 * crate::units::NANOS_PER_MIN));
        assert_eq!(b.close_ts, bars[14].close_ts);
        assert_eq!(
            (b.open, b.high, b.low, b.close),
            (Price(100), Price(200), Price(50), Price(101))
        );
        assert_eq!((b.volume, b.buy_volume, b.trades), (28, 14, 14));
        assert_eq!(aggregate_bars(&bars, 1), bars);
        // Canlı (artımlı) toplayıcı aynı barları, kova kapanır kapanmaz verir.
        let mut agg = BarAggregator::new(15);
        let mut live = Vec::new();
        for (i, x) in bars.iter().enumerate() {
            let done = agg.push(x);
            if i == 14 {
                assert_eq!(done.len(), 1, "15:14 barı kovayı kapatmalı");
            }
            live.extend(done);
        }
        live.extend(agg.flush());
        assert_eq!(live, a);
    }
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
