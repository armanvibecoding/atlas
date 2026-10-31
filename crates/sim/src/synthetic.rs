//! Deterministik sentetik veri (doğrulama testleri için). Tüm rastgelelik `ChaCha8` + açık seed.

use atlas_core::{Bar, InstrumentId, MarketEvent, MarketPayload, Price, Trade, UnixNanos};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// ±1 tick adımlı simetrik rastgele yürüyüş (martingale): beklenen fiyat değişimi tam 0.
pub fn random_walk_trades(
    seed: u64,
    inst: InstrumentId,
    start: UnixNanos,
    steps: usize,
    step_ns: u64,
    start_price: Price,
) -> Vec<MarketEvent> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut p = start_price;
    let mut out = Vec::with_capacity(steps);
    for k in 0..steps {
        let ts = start.plus((k as u64 + 1) * step_ns);
        p = if rng.random_bool(0.5) { p + 1 } else { p - 1 };
        if p.0 < 1 {
            p = Price(1);
        }
        out.push(MarketEvent::trade(
            inst,
            ts,
            Trade {
                price: p,
                size: 1,
                aggressor: None,
            },
        ));
    }
    out
}

/// İşlemleri `bar_ns` uzunluğunda barlara toplar. Bar olayı kapanış anında yayılır.
pub fn bars_from_trades(
    trades: &[MarketEvent],
    inst: InstrumentId,
    bar_ns: u64,
) -> Vec<MarketEvent> {
    let mut out = Vec::new();
    let mut cur: Option<Bar> = None;
    for ev in trades {
        let MarketPayload::Trade(t) = &ev.payload else {
            continue;
        };
        // İşlem `ts` anında; bar [open, close) — tam sınırdaki işlem yeni bara girer.
        let open_ts = UnixNanos(ev.ts.0 - ev.ts.0 % bar_ns);
        match &mut cur {
            Some(b) if b.open_ts == open_ts => {
                b.high = b.high.max(t.price);
                b.low = b.low.min(t.price);
                b.close = t.price;
                b.volume += t.size;
                b.trades += 1;
            }
            _ => {
                if let Some(b) = cur.take() {
                    out.push(MarketEvent::bar(inst, b));
                }
                cur = Some(Bar {
                    open_ts,
                    close_ts: open_ts.plus(bar_ns),
                    open: t.price,
                    high: t.price,
                    low: t.price,
                    close: t.price,
                    volume: t.size,
                    trades: 1,
                });
            }
        }
    }
    if let Some(b) = cur {
        out.push(MarketEvent::bar(inst, b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_aggregates() {
        let a = random_walk_trades(
            7,
            InstrumentId(0),
            UnixNanos(0),
            1000,
            1_000_000_000,
            Price(1000),
        );
        let b = random_walk_trades(
            7,
            InstrumentId(0),
            UnixNanos(0),
            1000,
            1_000_000_000,
            Price(1000),
        );
        let c = random_walk_trades(
            8,
            InstrumentId(0),
            UnixNanos(0),
            1000,
            1_000_000_000,
            Price(1000),
        );
        assert_eq!(a, b);
        assert_ne!(a, c);
        let bars = bars_from_trades(&a, InstrumentId(0), 60_000_000_000);
        // 1000 işlem, saniyede bir, 60 sn'lik barlar → 17 bar (ilk bar 1..59)
        assert_eq!(bars.len(), 17);
        let total: u64 = bars
            .iter()
            .map(|e| match &e.payload {
                MarketPayload::Bar(b) => b.volume,
                _ => 0,
            })
            .sum();
        assert_eq!(total, 1000);
        for w in bars.windows(2) {
            assert!(w[0].ts < w[1].ts);
        }
    }
}
