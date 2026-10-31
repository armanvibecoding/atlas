//! İstatistik yardımcıları: beklenti, Sharpe, çarpıklık/basıklık, Deflated Sharpe Ratio, PBO (CSCV).

use statrs::distribution::{ContinuousCDF, Normal};

pub fn mean(x: &[f64]) -> f64 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f64>() / x.len() as f64
    }
}

pub fn sd(x: &[f64]) -> f64 {
    let n = x.len();
    if n < 2 {
        return 0.0;
    }
    let m = mean(x);
    (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n as f64 - 1.0)).sqrt()
}

/// İşlem başı Sharpe (yıllıklandırılmamış).
pub fn sharpe(x: &[f64]) -> f64 {
    let s = sd(x);
    if s > 0.0 { mean(x) / s } else { 0.0 }
}

/// (çarpıklık, basıklık — normal = 3).
pub fn skew_kurt(x: &[f64]) -> (f64, f64) {
    let n = x.len() as f64;
    if n < 3.0 {
        return (0.0, 3.0);
    }
    let m = mean(x);
    let m2 = x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / n;
    if m2 <= 0.0 {
        return (0.0, 3.0);
    }
    let m3 = x.iter().map(|v| (v - m).powi(3)).sum::<f64>() / n;
    let m4 = x.iter().map(|v| (v - m).powi(4)).sum::<f64>() / n;
    (m3 / m2.powf(1.5), m4 / (m2 * m2))
}

fn std_normal() -> Option<Normal> {
    Normal::new(0.0, 1.0).ok()
}

pub fn phi(z: f64) -> f64 {
    std_normal().map_or(0.5, |n| n.cdf(z))
}

pub fn phi_inv(p: f64) -> f64 {
    std_normal().map_or(0.0, |n| n.inverse_cdf(p.clamp(1e-12, 1.0 - 1e-12)))
}

const EULER_GAMMA: f64 = std::f64::consts::EULER_GAMMA;

/// Bailey & López de Prado (2014): `n_trials` bağımsız denemenin en iyisinin beklenen Sharpe'ı.
pub fn expected_max_sharpe(var_sr: f64, n_trials: u64) -> f64 {
    if n_trials < 2 || var_sr <= 0.0 {
        return 0.0;
    }
    let n = n_trials as f64;
    var_sr.sqrt()
        * ((1.0 - EULER_GAMMA) * phi_inv(1.0 - 1.0 / n)
            + EULER_GAMMA * phi_inv(1.0 - 1.0 / (n * std::f64::consts::E)))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Dsr {
    pub sharpe: f64,
    pub sr0: f64,
    pub var_sr: f64,
    pub n_trials: u64,
    pub t: usize,
    pub skew: f64,
    pub kurt: f64,
    pub probability: f64,
}

/// Deflated Sharpe Ratio olasılığı. `trial_sharpes`: sicildeki tüm denemelerin Sharpe'ları.
/// V[SR] = max(denemelerin örnek varyansı, 1/T) — az deneme varken sıfıra çökmesin diye.
pub fn deflated_sharpe(x: &[f64], trial_sharpes: &[f64], n_trials: u64) -> Dsr {
    let t = x.len();
    let sr = sharpe(x);
    let (g3, g4) = skew_kurt(x);
    let sample_var = if trial_sharpes.len() >= 2 {
        let m = mean(trial_sharpes);
        trial_sharpes.iter().map(|s| (s - m).powi(2)).sum::<f64>()
            / (trial_sharpes.len() as f64 - 1.0)
    } else {
        0.0
    };
    let var_sr = sample_var.max(1.0 / (t.max(2) as f64));
    let sr0 = expected_max_sharpe(var_sr, n_trials);
    let denom = (1.0 - g3 * sr + (g4 - 1.0) / 4.0 * sr * sr)
        .max(1e-9)
        .sqrt();
    let z = (sr - sr0) * ((t.max(2) - 1) as f64).sqrt() / denom;
    Dsr {
        sharpe: sr,
        sr0,
        var_sr,
        n_trials,
        t,
        skew: g3,
        kurt: g4,
        probability: phi(z),
    }
}

/// k-li kombinasyonlar (sözlük sırası, deterministik).
pub fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    fn rec(start: usize, n: usize, k: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == k {
            out.push(cur.clone());
            return;
        }
        for i in start..n {
            cur.push(i);
            rec(i + 1, n, k, cur, out);
            cur.pop();
        }
    }
    let mut out = Vec::new();
    rec(0, n, k, &mut Vec::new(), &mut out);
    out
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Pbo {
    pub configs: usize,
    pub blocks: usize,
    pub combinations: usize,
    pub probability: f64,
}

/// Combinatorially Symmetric Cross-Validation (Bailey vd. 2015).
/// `perf[s][c]`: blok `s`'de konfigürasyon `c`'nin performansı (toplam net).
pub fn pbo_cscv(perf: &[Vec<f64>]) -> Pbo {
    let s = perf.len();
    let m = perf.first().map_or(0, Vec::len);
    if s < 2 || m < 2 {
        return Pbo {
            configs: m,
            blocks: s,
            combinations: 0,
            probability: 1.0,
        };
    }
    let combos = combinations(s, s / 2);
    let mut below = 0usize;
    for is in &combos {
        let oos: Vec<usize> = (0..s).filter(|b| !is.contains(b)).collect();
        let score = |blocks: &[usize], c: usize| blocks.iter().map(|b| perf[*b][c]).sum::<f64>();
        let best = (0..m)
            .max_by(|a, b| score(is, *a).total_cmp(&score(is, *b)).then(b.cmp(a)))
            .unwrap_or(0);
        let oos_best = score(&oos, best);
        // OOS'ta en iyinin sırası (1 = en kötü). Eşitlikte orta sıra.
        let lower = (0..m).filter(|c| score(&oos, *c) < oos_best).count() as f64;
        let equal = (0..m).filter(|c| score(&oos, *c) == oos_best).count() as f64;
        let rank = lower + (equal + 1.0) / 2.0;
        let omega = rank / (m as f64 + 1.0);
        if omega <= 0.5 {
            below += 1;
        }
    }
    Pbo {
        configs: m,
        blocks: s,
        combinations: combos.len(),
        probability: below as f64 / combos.len() as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_stats() {
        let x = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(mean(&x), 2.5);
        assert!((sd(&x) - 1.290_994).abs() < 1e-5);
        let (s, k) = skew_kurt(&x);
        assert!(s.abs() < 1e-12);
        assert!((k - 1.64).abs() < 1e-9);
        assert!((phi(0.0) - 0.5).abs() < 1e-12);
        assert!((phi_inv(0.975) - 1.959_964).abs() < 1e-5);
    }

    #[test]
    fn expected_max_grows_with_trials() {
        assert_eq!(expected_max_sharpe(0.01, 1), 0.0);
        let a = expected_max_sharpe(0.01, 10);
        let b = expected_max_sharpe(0.01, 1000);
        assert!(b > a && a > 0.0);
        // Bailey & López de Prado tablosu: V=1, N=10 → ≈1.57
        assert!((expected_max_sharpe(1.0, 10) - 1.57).abs() < 0.02);
    }

    #[test]
    fn dsr_penalizes_many_trials() {
        let x: Vec<f64> = (0..400)
            .map(|i| if i % 3 == 0 { -1.0 } else { 0.9 })
            .collect();
        let few = deflated_sharpe(&x, &[], 1);
        let many = deflated_sharpe(&x, &[0.0, 0.3, -0.2, 0.1], 10_000);
        assert!(few.probability > many.probability);
    }

    #[test]
    fn pbo_detects_overfit_and_robust() {
        // Sağlam: config 0 her blokta en iyi → PBO 0.
        let robust: Vec<Vec<f64>> = (0..8).map(|_| vec![3.0, 1.0, 2.0, 0.5]).collect();
        assert_eq!(pbo_cscv(&robust).probability, 0.0);
        // Aşırı uyum: her config yalnız kendi bloğunda iyi → seçilen OOS'ta kötü.
        let over: Vec<Vec<f64>> = (0..8)
            .map(|s| (0..8).map(|c| if c == s { 10.0 } else { -1.0 }).collect())
            .collect();
        assert!(pbo_cscv(&over).probability > 0.9);
        assert_eq!(combinations(8, 4).len(), 70);
    }
}
