//! Triple-barrier etiketleme (López de Prado).
//!
//! Aday: giriş barı `entry_idx` (karar o barın **kapanışında**, referans fiyat = kapanış), yön,
//! hedef/stop mesafesi (tick) ve zaman bariyeri (bar sayısı). Yol `entry_idx+1 ..= entry_idx+max_bars`
//! barlarının yüksek/düşükleriyle taranır:
//! - hedef önce dokunursa `+1` ([`Outcome::Profit`]),
//! - stop önce dokunursa `−1` ([`Outcome::Stop`]),
//! - **aynı barda ikisi birden** dokunursa `−1` (karamsar; simülatörün "aynı mumda stop+hedef → stop"
//!   kuralıyla tutarlı, §4.4),
//! - hiçbiri dokunmazsa zaman bariyerinde `0` ([`Outcome::Time`]).
//!
//! Uzun pozisyonda üst bariyer = hedef, alt = stop; kısada tersi. Fiyatlar tick (`Price(i64)`).
//! Etiket geleceği kullanır; bu yüzden **yalnız eğitimde** kullanılır. Veri zaman bariyerinden önce
//! biterse ve bariyer dokunulmamışsa etiket bilinemez → `None` (yanlı "0" yazılmaz).

use crate::MlError;
use atlas_core::{Bar, Price, Side};
use serde::{Deserialize, Serialize};

/// Bariyer tanımı: giriş fiyatından tick mesafeleri ve bar cinsinden zaman bariyeri.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Barrier {
    /// Hedef mesafesi (tick, > 0).
    pub profit_ticks: i64,
    /// Stop mesafesi (tick, > 0).
    pub stop_ticks: i64,
    /// Zaman bariyeri: girişten sonra en çok kaç bar (> 0).
    pub max_bars: usize,
}

impl Barrier {
    fn validate(&self) -> Result<(), MlError> {
        if self.profit_ticks <= 0 || self.stop_ticks <= 0 || self.max_bars == 0 {
            return Err(MlError::InvalidBarrier(format!(
                "hedef {}t, stop {}t, zaman {} bar: hepsi > 0 olmalı",
                self.profit_ticks, self.stop_ticks, self.max_bars
            )));
        }
        Ok(())
    }
}

/// Birincil sinyalin ürettiği aday giriş.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Karar barının indeksi (giriş referansı = bu barın kapanışı).
    pub entry_idx: usize,
    pub side: Side,
    pub barrier: Barrier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Hedef önce: +1.
    Profit,
    /// Stop önce (ya da aynı barda ikisi): −1.
    Stop,
    /// Zaman bariyeri: 0.
    Time,
}

impl Outcome {
    /// Klasik triple-barrier etiketi: +1 / −1 / 0.
    pub const fn value(self) -> i8 {
        match self {
            Outcome::Profit => 1,
            Outcome::Stop => -1,
            Outcome::Time => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub entry_idx: usize,
    /// Bariyerin dokunulduğu (ya da zaman bariyerinin dolduğu) bar.
    pub exit_idx: usize,
    pub side: Side,
    pub outcome: Outcome,
    pub entry_price: Price,
    /// Bariyer fiyatı; boşluk (gap) bariyeri aşarak açtıysa açılış fiyatı; zaman bariyerinde kapanış.
    pub exit_price: Price,
    /// Yön işaretli getiri (tick).
    pub ret_ticks: i64,
}

/// Tek adayı etiketler. Geçersiz bariyer → hata; giriş aralık dışıysa ya da veri etiketi belirlemeye
/// yetmiyorsa → `Ok(None)`.
pub fn triple_barrier(bars: &[Bar], c: &Candidate) -> Result<Option<Label>, MlError> {
    c.barrier.validate()?;
    let Some(entry_bar) = bars.get(c.entry_idx) else {
        return Ok(None);
    };
    let entry = entry_bar.close;
    let (target, stop) = match c.side {
        Side::Buy => (entry + c.barrier.profit_ticks, entry - c.barrier.stop_ticks),
        Side::Sell => (entry - c.barrier.profit_ticks, entry + c.barrier.stop_ticks),
    };
    let last = c.entry_idx.saturating_add(c.barrier.max_bars);
    let make = |exit_idx: usize, outcome: Outcome, exit_price: Price| Label {
        entry_idx: c.entry_idx,
        exit_idx,
        side: c.side,
        outcome,
        entry_price: entry,
        exit_price,
        ret_ticks: (exit_price - entry).saturating_mul(i64::from(c.side.sign())),
    };
    for k in (c.entry_idx + 1)..=last {
        let Some(b) = bars.get(k) else {
            // Veri bitti, bariyer dokunulmadı: etiket bilinemez.
            return Ok(None);
        };
        let (hit_stop, hit_target, stop_px, target_px) = match c.side {
            Side::Buy => (
                b.low <= stop,
                b.high >= target,
                // Boşlukla stop altında açıldıysa dolum açılıştan (simülatörle aynı).
                if b.open <= stop { b.open } else { stop },
                if b.open >= target { b.open } else { target },
            ),
            Side::Sell => (
                b.high >= stop,
                b.low <= target,
                if b.open >= stop { b.open } else { stop },
                if b.open <= target { b.open } else { target },
            ),
        };
        // Aynı barda ikisi → stop (karamsar).
        if hit_stop {
            return Ok(Some(make(k, Outcome::Stop, stop_px)));
        }
        if hit_target {
            return Ok(Some(make(k, Outcome::Profit, target_px)));
        }
        if k == last {
            return Ok(Some(make(k, Outcome::Time, b.close)));
        }
    }
    Ok(None)
}

/// Tüm adayları etiketler; belirlenemeyenler atlanır. Çıktı aday sırasındadır.
pub fn label_all(bars: &[Bar], candidates: &[Candidate]) -> Result<Vec<Label>, MlError> {
    let mut out = Vec::with_capacity(candidates.len());
    for c in candidates {
        if let Some(l) = triple_barrier(bars, c)? {
            out.push(l);
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use atlas_core::UnixNanos;

    /// (açılış, yüksek, düşük, kapanış) dörtlülerinden 1 dakikalık barlar.
    pub(crate) fn bars(ohlc: &[(i64, i64, i64, i64)]) -> Vec<Bar> {
        ohlc.iter()
            .enumerate()
            .map(|(i, &(o, h, l, c))| {
                let open_ts = UnixNanos(1_000 * 60_000_000_000 + i as u64 * 60_000_000_000);
                Bar {
                    open_ts,
                    close_ts: open_ts.plus(60_000_000_000),
                    open: Price(o),
                    high: Price(h),
                    low: Price(l),
                    close: Price(c),
                    volume: 10,
                    buy_volume: 5,
                    trades: 1,
                }
            })
            .collect()
    }

    fn cand(side: Side, p: i64, s: i64, n: usize) -> Candidate {
        Candidate {
            entry_idx: 0,
            side,
            barrier: Barrier {
                profit_ticks: p,
                stop_ticks: s,
                max_bars: n,
            },
        }
    }

    #[test]
    fn target_first_long_and_short() {
        let b = bars(&[
            (100, 100, 100, 100),
            (100, 103, 99, 102),
            (102, 106, 101, 105),
            (105, 105, 90, 91),
        ]);
        let l = triple_barrier(&b, &cand(Side::Buy, 5, 5, 10))
            .unwrap()
            .unwrap();
        assert_eq!(l.outcome, Outcome::Profit);
        assert_eq!(l.outcome.value(), 1);
        assert_eq!(l.exit_idx, 2);
        assert_eq!(l.exit_price, Price(105));
        assert_eq!(l.ret_ticks, 5);

        // Kısa: hedef aşağıda (95), stop yukarıda (110).
        let b = bars(&[(100, 100, 100, 100), (100, 101, 97, 98), (98, 99, 94, 95)]);
        let l = triple_barrier(&b, &cand(Side::Sell, 5, 10, 10))
            .unwrap()
            .unwrap();
        assert_eq!(
            (l.outcome, l.exit_idx, l.ret_ticks),
            (Outcome::Profit, 2, 5)
        );
    }

    #[test]
    fn stop_first() {
        let b = bars(&[
            (100, 100, 100, 100),
            (100, 101, 96, 97),
            (97, 98, 94, 95),
            (95, 120, 95, 119),
        ]);
        let l = triple_barrier(&b, &cand(Side::Buy, 5, 5, 10))
            .unwrap()
            .unwrap();
        assert_eq!(l.outcome, Outcome::Stop);
        assert_eq!(l.outcome.value(), -1);
        assert_eq!((l.exit_idx, l.exit_price, l.ret_ticks), (2, Price(95), -5));

        // Kısada stop yukarıda.
        let l = triple_barrier(&b, &cand(Side::Sell, 50, 10, 10))
            .unwrap()
            .unwrap();
        assert_eq!(
            (l.outcome, l.exit_idx, l.ret_ticks),
            (Outcome::Stop, 3, -10)
        );
    }

    #[test]
    fn same_bar_both_is_stop() {
        let b = bars(&[(100, 100, 100, 100), (100, 110, 90, 100)]);
        for side in [Side::Buy, Side::Sell] {
            let l = triple_barrier(&b, &cand(side, 5, 5, 10)).unwrap().unwrap();
            assert_eq!(l.outcome, Outcome::Stop, "{side:?}");
            assert_eq!(l.exit_idx, 1);
            assert_eq!(l.ret_ticks, -5);
        }
    }

    #[test]
    fn gap_through_stop_fills_at_open() {
        let b = bars(&[(100, 100, 100, 100), (90, 91, 88, 89)]);
        let l = triple_barrier(&b, &cand(Side::Buy, 5, 5, 10))
            .unwrap()
            .unwrap();
        assert_eq!(
            (l.outcome, l.exit_price, l.ret_ticks),
            (Outcome::Stop, Price(90), -10)
        );
    }

    #[test]
    fn time_barrier() {
        let b = bars(&[
            (100, 100, 100, 100),
            (100, 102, 98, 101),
            (101, 103, 99, 102),
            (102, 104, 100, 103),
            (103, 200, 0, 103),
        ]);
        let l = triple_barrier(&b, &cand(Side::Buy, 5, 5, 3))
            .unwrap()
            .unwrap();
        assert_eq!(l.outcome, Outcome::Time);
        assert_eq!(l.outcome.value(), 0);
        assert_eq!((l.exit_idx, l.exit_price, l.ret_ticks), (3, Price(103), 3));
    }

    #[test]
    fn insufficient_data_and_invalid_barrier() {
        let b = bars(&[(100, 100, 100, 100), (100, 101, 99, 100)]);
        // Zaman bariyeri verinin ötesinde, bariyer dokunulmamış → bilinmez.
        assert_eq!(triple_barrier(&b, &cand(Side::Buy, 5, 5, 3)).unwrap(), None);
        // Giriş aralık dışı.
        let mut c = cand(Side::Buy, 5, 5, 1);
        c.entry_idx = 9;
        assert_eq!(triple_barrier(&b, &c).unwrap(), None);
        assert!(triple_barrier(&b, &cand(Side::Buy, 0, 5, 3)).is_err());
        assert!(triple_barrier(&b, &cand(Side::Buy, 5, 5, 0)).is_err());
        // Bariyer veri bitmeden dokunduysa zaman bariyeri veri dışında olsa da etiket bilinir.
        let b = bars(&[(100, 100, 100, 100), (100, 106, 99, 105)]);
        let l = triple_barrier(&b, &cand(Side::Buy, 5, 5, 50))
            .unwrap()
            .unwrap();
        assert_eq!(l.outcome, Outcome::Profit);
        assert_eq!(
            label_all(&b, &[cand(Side::Buy, 5, 5, 50)]).unwrap().len(),
            1
        );
    }
}
