//! Portföy (§4.10): sapma dedektörü (CUSUM + SPRT), terfi/düşürme kararı, zamanla azalan Thompson
//! allocator, rejim tespiti.
//!
//! Sapma: canlı (gölge/paper) işlem sonuçları gauntlet'in tahmin ettiği dağılıma (μ₀, σ) karşı
//! izlenir. İşlem sonuçları R biriminde (net / risk) verilir; böylece boyuttan bağımsızdır.

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Gauntlet'in in-sample'dan beklediği dağılım (R/işlem).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Expected {
    pub mean_r: f64,
    pub sd_r: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Kanıt yeterli değil; izlemeye devam.
    Continue,
    /// Performans beklentiyle tutarlı (H₀ kabul) → terfi kanıtı.
    Promote,
    /// Beklentinin anlamlı altında (H₁ kabul) → bir basamak düşür.
    Demote,
}

/// Wald SPRT: H₀: μ = μ₀ (gauntlet beklentisi) vs H₁: μ = μ₁ = μ₀ − δσ (bozulmuş). Normal yaklaşım.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sprt {
    pub expected: Expected,
    /// Bozulma büyüklüğü (σ cinsinden).
    pub delta_sd: f64,
    pub alpha: f64,
    pub beta: f64,
    pub min_trades: usize,
    pub llr: f64,
    pub n: usize,
}

impl Sprt {
    pub fn new(
        expected: Expected,
        delta_sd: f64,
        alpha: f64,
        beta: f64,
        min_trades: usize,
    ) -> Self {
        Sprt {
            expected,
            delta_sd,
            alpha,
            beta,
            min_trades,
            llr: 0.0,
            n: 0,
        }
    }

    /// Varsayılan: δ = 0.5σ, α = β = 0.05, en az 30 işlem.
    pub fn default_for(expected: Expected) -> Self {
        Self::new(expected, 0.5, 0.05, 0.05, 30)
    }

    pub fn bounds(&self) -> (f64, f64) {
        // (alt = H₀ kabul, üst = H₁ kabul)
        (
            (self.beta / (1.0 - self.alpha)).ln(),
            ((1.0 - self.beta) / self.alpha).ln(),
        )
    }

    pub fn update(&mut self, r: f64) -> Verdict {
        let s = self.expected.sd_r.max(1e-9);
        let m0 = self.expected.mean_r;
        let m1 = m0 - self.delta_sd * s;
        // log [f1(x)/f0(x)] normal yoğunluklar için
        self.llr += ((r - m0).powi(2) - (r - m1).powi(2)) / (2.0 * s * s);
        self.n += 1;
        self.verdict()
    }

    pub fn verdict(&self) -> Verdict {
        if self.n < self.min_trades {
            return Verdict::Continue;
        }
        let (lo, hi) = self.bounds();
        if self.llr >= hi {
            Verdict::Demote
        } else if self.llr <= lo {
            Verdict::Promote
        } else {
            Verdict::Continue
        }
    }
}

/// Tek yönlü (aşağı) CUSUM: standartlaştırılmış getirilerde ortalama düşüşünü erken yakalar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cusum {
    pub expected: Expected,
    /// Tolerans k (σ cinsinden).
    pub k: f64,
    /// Alarm eşiği h (σ cinsinden).
    pub h: f64,
    pub s: f64,
    pub alarmed: bool,
}

impl Cusum {
    pub fn new(expected: Expected, k: f64, h: f64) -> Self {
        Cusum {
            expected,
            k,
            h,
            s: 0.0,
            alarmed: false,
        }
    }

    pub fn default_for(expected: Expected) -> Self {
        Self::new(expected, 0.5, 5.0)
    }

    /// `true` = alarm (yeni tetiklendi).
    pub fn update(&mut self, r: f64) -> bool {
        let z = (r - self.expected.mean_r) / self.expected.sd_r.max(1e-9);
        self.s = (self.s - z - self.k).max(0.0);
        if self.s > self.h && !self.alarmed {
            self.alarmed = true;
            return true;
        }
        false
    }
}

/// Bir stratejinin canlı izleyicisi: hem SPRT hem CUSUM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DriftMonitor {
    pub sprt: Sprt,
    pub cusum: Cusum,
    pub results_r: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriftUpdate {
    pub verdict: Verdict,
    pub cusum_alarm: bool,
    pub n: usize,
    pub mean_r: f64,
    pub llr: f64,
}

impl DriftMonitor {
    pub fn new(expected: Expected) -> Self {
        DriftMonitor {
            sprt: Sprt::default_for(expected),
            cusum: Cusum::default_for(expected),
            results_r: Vec::new(),
        }
    }

    pub fn update(&mut self, r: f64) -> DriftUpdate {
        self.results_r.push(r);
        let verdict = self.sprt.update(r);
        let alarm = self.cusum.update(r);
        let n = self.results_r.len();
        DriftUpdate {
            // CUSUM alarmı da düşürme sebebidir (SPRT'den önce gelebilir).
            verdict: if alarm { Verdict::Demote } else { verdict },
            cusum_alarm: alarm,
            n,
            mean_r: self.results_r.iter().sum::<f64>() / n as f64,
            llr: self.sprt.llr,
        }
    }
}

/// Zamanla azalan ağırlıklı Thompson sampling allocator. Her strateji için normal-gamma yerine
/// basit yaklaşım: ortalama R'nin sonsal ~ N(m, s²/n_eff), n_eff = azalan ağırlıklı örnek sayısı.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Allocator {
    /// Strateji → (ağırlıklı toplam, ağırlıklı kare toplam, ağırlıklı sayı)
    stats: BTreeMap<String, (f64, f64, f64)>,
    /// Her yeni gözlemde eski ağırlıkların çarpanı (ör. 0.99).
    pub decay: f64,
}

impl Allocator {
    pub fn new(decay: f64) -> Self {
        Allocator {
            stats: BTreeMap::new(),
            decay,
        }
    }

    pub fn observe(&mut self, strategy: &str, r: f64) {
        let d = self.decay;
        let e = self
            .stats
            .entry(strategy.to_string())
            .or_insert((0.0, 0.0, 0.0));
        *e = (e.0 * d + r, e.1 * d + r * r, e.2 * d + 1.0);
    }

    pub fn ensure(&mut self, strategy: &str) {
        self.stats
            .entry(strategy.to_string())
            .or_insert((0.0, 0.0, 0.0));
    }

    /// Thompson örneklemesiyle risk payları (toplam 1). Örneği ≤ 0 olanlar 0 pay alır; hepsi ≤ 0 ise eşit.
    pub fn weights(&self, seed: u64) -> BTreeMap<String, f64> {
        use rand::Rng;
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut draws: BTreeMap<String, f64> = BTreeMap::new();
        for (k, (s, s2, n)) in &self.stats {
            let n_eff = n.max(1.0);
            let mean = s / n_eff;
            let var = (s2 / n_eff - mean * mean).max(0.25);
            let sd = (var / n_eff).sqrt();
            let z = (0..12).map(|_| rng.random::<f64>()).sum::<f64>() - 6.0;
            draws.insert(k.clone(), (mean + sd * z).max(0.0));
        }
        let tot: f64 = draws.values().sum();
        let m = draws.len().max(1) as f64;
        draws
            .into_iter()
            .map(|(k, v)| (k, if tot > 0.0 { v / tot } else { 1.0 / m }))
            .collect()
    }
}

/// Rejim: oynaklık (ATR/fiyat medyanın üstü/altı) × trend (fiyat EMA200 üstü/altı). Gauntlet kapı 8 ile aynı tanım.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Regime {
    pub high_vol: bool,
    pub up_trend: bool,
}

impl Regime {
    pub fn classify(atr: f64, close: f64, ema200: f64, atr_rel_median: f64) -> Option<Regime> {
        if atr.is_nan() || ema200.is_nan() || close <= 0.0 {
            return None;
        }
        Some(Regime {
            high_vol: atr / close > atr_rel_median,
            up_trend: close > ema200,
        })
    }

    pub fn label(&self) -> String {
        format!(
            "{}/{}",
            if self.high_vol {
                "vol_yüksek"
            } else {
                "vol_düşük"
            },
            if self.up_trend {
                "trend_yukarı"
            } else {
                "trend_aşağı"
            }
        )
    }

    /// Gauntlet'te bu rejimde PnL negatif ya da hiç işlem yoksa strateji kısılır (boyut çarpanı).
    pub fn throttle(&self, pnl_by_regime: &BTreeMap<String, f64>) -> f64 {
        match pnl_by_regime.get(&self.label()) {
            Some(p) if *p > 0.0 => 1.0,
            Some(_) => 0.0,
            None => 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal_seq(seed: u64, n: usize, mean: f64, sd: f64) -> Vec<f64> {
        use rand::Rng;
        let mut r = ChaCha8Rng::seed_from_u64(seed);
        (0..n)
            .map(|_| mean + sd * ((0..12).map(|_| r.random::<f64>()).sum::<f64>() - 6.0))
            .collect()
    }

    #[test]
    fn sprt_promotes_consistent_and_demotes_degraded() {
        let exp = Expected {
            mean_r: 0.2,
            sd_r: 1.0,
        };
        let mut ok = Sprt::default_for(exp);
        let mut v = Verdict::Continue;
        for r in normal_seq(1, 400, 0.2, 1.0) {
            v = ok.update(r);
            if v != Verdict::Continue {
                break;
            }
        }
        assert_eq!(v, Verdict::Promote);
        let mut bad = Sprt::default_for(exp);
        let mut v = Verdict::Continue;
        for r in normal_seq(2, 400, -0.4, 1.0) {
            v = bad.update(r);
            if v != Verdict::Continue {
                break;
            }
        }
        assert_eq!(v, Verdict::Demote);
        // Asgari işlem sayısından önce karar yok.
        let mut early = Sprt::default_for(exp);
        for _ in 0..29 {
            assert_eq!(early.update(-5.0), Verdict::Continue);
        }
    }

    #[test]
    fn cusum_alarms_on_shift_only() {
        let exp = Expected {
            mean_r: 0.1,
            sd_r: 1.0,
        };
        let mut c = Cusum::default_for(exp);
        assert!(
            !normal_seq(3, 300, 0.1, 1.0)
                .into_iter()
                .any(|r| c.update(r))
        );
        let mut d = Cusum::default_for(exp);
        assert!(
            normal_seq(4, 300, -1.0, 1.0)
                .into_iter()
                .any(|r| d.update(r))
        );
    }

    #[test]
    fn allocator_prefers_winner_and_is_deterministic() {
        let mut a = Allocator::new(0.99);
        for r in normal_seq(5, 200, 0.5, 1.0) {
            a.observe("iyi", r);
        }
        for r in normal_seq(6, 200, -0.2, 1.0) {
            a.observe("kötü", r);
        }
        let w = a.weights(7);
        assert!(w["iyi"] > w["kötü"]);
        assert!((w.values().sum::<f64>() - 1.0).abs() < 1e-9);
        assert_eq!(a.weights(7), w);
    }

    #[test]
    fn regime_throttle() {
        let r = Regime::classify(10.0, 1000.0, 900.0, 0.005).unwrap();
        assert_eq!(r.label(), "vol_yüksek/trend_yukarı");
        let mut m = BTreeMap::new();
        m.insert("vol_yüksek/trend_yukarı".to_string(), -5.0);
        assert_eq!(r.throttle(&m), 0.0);
        m.insert("vol_yüksek/trend_yukarı".to_string(), 5.0);
        assert_eq!(r.throttle(&m), 1.0);
        assert_eq!(r.throttle(&BTreeMap::new()), 0.5);
    }
}
