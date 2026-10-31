//! Veri kalite monitörü: boşluk, sıra dışı/yinelenen zaman damgası, tutarsız OHLC, sıfır hacim,
//! kilitli/çapraz kitap. Hata = olay (`data_quality`); **sessiz düzeltme yok** (§4.1).

use atlas_core::{Bar, Event, EventKind, Quote, SessionSpec, UnixNanos};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "issue")]
pub enum QualityIssue {
    OutOfOrder {
        prev: UnixNanos,
        ts: UnixNanos,
    },
    Duplicate {
        ts: UnixNanos,
    },
    /// Seans açıkken eksik bar.
    Gap {
        after: UnixNanos,
        before: UnixNanos,
        missing_open_minutes: u64,
    },
    BadOhlc {
        ts: UnixNanos,
    },
    ZeroVolume {
        ts: UnixNanos,
    },
    LockedBook {
        ts: UnixNanos,
    },
    CrossedBook {
        ts: UnixNanos,
    },
}

impl QualityIssue {
    pub fn to_event(&self, symbol: &str, ts: UnixNanos) -> Event {
        Event::new(
            ts,
            EventKind::DataQuality,
            serde_json::json!({"symbol": symbol, "detail": self}),
        )
    }
}

/// 1 dakikalık barları denetler. Seans kapalıyken eksik bar boşluk sayılmaz.
pub fn check_bars(bars: &[Bar], session: &SessionSpec, interval_ns: u64) -> Vec<QualityIssue> {
    let mut out = Vec::new();
    for b in bars {
        if b.high < b.low
            || b.high < b.open
            || b.high < b.close
            || b.low > b.open
            || b.low > b.close
            || b.close_ts.since(b.open_ts) != interval_ns
        {
            out.push(QualityIssue::BadOhlc { ts: b.open_ts });
        }
        if b.volume == 0 {
            out.push(QualityIssue::ZeroVolume { ts: b.open_ts });
        }
    }
    for w in bars.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if b.open_ts < a.open_ts {
            out.push(QualityIssue::OutOfOrder {
                prev: a.open_ts,
                ts: b.open_ts,
            });
        } else if b.open_ts == a.open_ts {
            out.push(QualityIssue::Duplicate { ts: b.open_ts });
        } else if b.open_ts > a.close_ts {
            let missing = session.open_minutes_between(a.close_ts, b.open_ts);
            if missing > 0 {
                out.push(QualityIssue::Gap {
                    after: a.open_ts,
                    before: b.open_ts,
                    missing_open_minutes: missing,
                });
            }
        }
    }
    out
}

pub fn check_quotes(quotes: &[(UnixNanos, Quote)]) -> Vec<QualityIssue> {
    let mut out = Vec::new();
    for (ts, q) in quotes {
        if q.bid == q.ask {
            out.push(QualityIssue::LockedBook { ts: *ts });
        } else if q.bid > q.ask {
            out.push(QualityIssue::CrossedBook { ts: *ts });
        }
    }
    for w in quotes.windows(2) {
        if w[1].0 < w[0].0 {
            out.push(QualityIssue::OutOfOrder {
                prev: w[0].0,
                ts: w[1].0,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::Price;

    fn b(min: u64, vol: u64) -> Bar {
        let n = 60_000_000_000;
        Bar {
            open_ts: UnixNanos(min * n),
            close_ts: UnixNanos((min + 1) * n),
            open: Price(10),
            high: Price(11),
            low: Price(9),
            close: Price(10),
            volume: vol,
            trades: 1,
        }
    }

    #[test]
    fn finds_gap_zero_volume_duplicate_and_bad_ohlc() {
        let mut bad = b(7, 1);
        bad.high = Price(8);
        let bars = vec![b(0, 1), b(1, 0), b(1, 1), b(5, 1), bad];
        let issues = check_bars(&bars, &SessionSpec::Always, 60_000_000_000);
        assert!(issues.contains(&QualityIssue::ZeroVolume {
            ts: UnixNanos(60_000_000_000)
        }));
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, QualityIssue::Duplicate { .. }))
        );
        assert!(issues.iter().any(|i| matches!(
            i,
            QualityIssue::Gap {
                missing_open_minutes: 3,
                ..
            }
        )));
        assert!(
            issues
                .iter()
                .any(|i| matches!(i, QualityIssue::BadOhlc { .. }))
        );
        let mut ooo = vec![b(3, 1), b(2, 1)];
        ooo.truncate(2);
        assert!(
            check_bars(&ooo, &SessionSpec::Always, 60_000_000_000)
                .iter()
                .any(|i| matches!(i, QualityIssue::OutOfOrder { .. }))
        );
    }

    #[test]
    fn book_checks() {
        let q = |b: i64, a: i64| Quote {
            bid: Price(b),
            ask: Price(a),
            bid_size: 1,
            ask_size: 1,
        };
        let v = check_quotes(&[
            (UnixNanos(1), q(10, 10)),
            (UnixNanos(2), q(11, 10)),
            (UnixNanos(3), q(9, 10)),
        ]);
        assert_eq!(v.len(), 2);
    }
}
