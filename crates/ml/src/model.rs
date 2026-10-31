//! L2 düzenlileştirilmiş lojistik regresyon (Newton/IRLS) + serileştirilebilir meta model.
//!
//! - Özellikler eğitim verisinin ortalama/std'siyle standartlaştırılır (std ≈ 0 → 1; sabit sütun
//!   modele katkı vermez). Standartlaştırma parametreleri modelle birlikte saklanır.
//! - Amaç: `Σ [y·z − softplus(z)] − (λ/2)·‖w‖²` (kesişim cezasız) maksimize edilir.
//! - Her adım `H δ = g` Cholesky ile çözülür; amaç iyileşmezse adım yarıya indirilir (en çok 30 kez).
//! - Rastgelelik yok; toplama sırası sabit → aynı veri, aynı katsayılar (bit düzeyinde, aynı platformda).

use crate::MlError;
use crate::cv::{CvConfig, CvReport, cross_validate};
use crate::dataset::{Dataset, Sample, Transform};
use atlas_core::{Side, UnixNanos};
use serde::{Deserialize, Serialize};

/// Model dosya biçimi sürümü. Biçim ya da hesap mantığı değişince artar.
pub const MODEL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LogitConfig {
    /// L2 ceza katsayısı λ (> 0), toplam log-olabilirliğe göre (örnek başı değil).
    pub l2: f64,
    pub max_iter: usize,
    /// Yakınsama: adımın en büyük mutlak bileşeni bunun altında.
    pub tol: f64,
}

impl Default for LogitConfig {
    fn default() -> Self {
        LogitConfig {
            l2: 1.0,
            max_iter: 100,
            tol: 1e-10,
        }
    }
}

impl LogitConfig {
    fn validate(&self) -> Result<(), MlError> {
        if !(self.l2.is_finite() && self.l2 > 0.0) {
            return Err(MlError::InvalidInput(format!("l2 > 0 olmalı: {}", self.l2)));
        }
        if self.max_iter == 0 || !(self.tol.is_finite() && self.tol > 0.0) {
            return Err(MlError::InvalidInput(
                "max_iter > 0 ve tol > 0 olmalı".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogisticModel {
    #[serde(with = "exact")]
    pub mean: Vec<f64>,
    #[serde(with = "exact")]
    pub std: Vec<f64>,
    /// Standartlaştırılmış özellik katsayıları.
    #[serde(with = "exact")]
    pub weights: Vec<f64>,
    #[serde(with = "exact_one")]
    pub bias: f64,
    pub iterations: usize,
    pub converged: bool,
}

/// Model parametreleri JSON'da **metin** olarak yazılır: Rust'ın en kısa gidiş-dönüş biçimi +
/// `str::parse` (doğru yuvarlamalı) → okunan katsayılar bit düzeyinde aynıdır. (serde_json'un
/// varsayılan sayı ayrıştırıcısı son bitte yuvarlama hatası yapabilir.)
mod exact {
    use serde::{Deserialize, Deserializer, Serializer, de::Error, ser::SerializeSeq};

    pub fn serialize<S: Serializer>(v: &[f64], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(v.len()))?;
        for x in v {
            seq.serialize_element(&format!("{x:?}"))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
        Vec::<String>::deserialize(d)?
            .iter()
            .map(|t| t.parse::<f64>().map_err(D::Error::custom))
            .collect()
    }
}

mod exact_one {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{v:?}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        String::deserialize(d)?
            .parse::<f64>()
            .map_err(D::Error::custom)
    }
}

fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

fn softplus(z: f64) -> f64 {
    z.max(0.0) + (-z.abs()).exp().ln_1p()
}

/// Simetrik pozitif tanımlı `a` (n×n, satır sıralı) için `a x = b` (Cholesky).
fn cholesky_solve(a: &[f64], b: &[f64], n: usize) -> Result<Vec<f64>, MlError> {
    let mut l = vec![0.0f64; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if !(s.is_finite() && s > 0.0) {
                    return Err(MlError::Singular);
                }
                l[i * n + i] = s.sqrt();
            } else {
                l[i * n + j] = s / l[j * n + j];
            }
        }
    }
    // L y = b
    let mut y = vec![0.0f64; n];
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= l[i * n + k] * y[k];
        }
        y[i] = s / l[i * n + i];
    }
    // Lᵀ x = y
    let mut x = vec![0.0f64; n];
    for i in (0..n).rev() {
        let mut s = y[i];
        for k in (i + 1)..n {
            s -= l[k * n + i] * x[k];
        }
        x[i] = s / l[i * n + i];
    }
    Ok(x)
}

impl LogisticModel {
    /// Örneklerden eğitir. En az bir pozitif ve bir negatif örnek gerekir.
    pub fn fit(samples: &[&Sample], cfg: &LogitConfig) -> Result<Self, MlError> {
        cfg.validate()?;
        let Some(first) = samples.first() else {
            return Err(MlError::InvalidInput("boş eğitim kümesi".into()));
        };
        let d = first.x.len();
        if samples
            .iter()
            .any(|s| s.x.len() != d || s.x.iter().any(|v| !v.is_finite()))
        {
            return Err(MlError::InvalidInput(
                "özellik boyutu tutarsız ya da sonlu değil".into(),
            ));
        }
        let n_pos = samples.iter().filter(|s| s.y).count();
        if n_pos == 0 || n_pos == samples.len() {
            return Err(MlError::SingleClass);
        }
        let n = samples.len() as f64;

        // Standartlaştırma (popülasyon std).
        let mut mean = vec![0.0f64; d];
        for s in samples {
            for (m, v) in mean.iter_mut().zip(&s.x) {
                *m += v;
            }
        }
        for m in &mut mean {
            *m /= n;
        }
        let mut var = vec![0.0f64; d];
        for s in samples {
            for ((q, v), m) in var.iter_mut().zip(&s.x).zip(&mean) {
                let c = v - m;
                *q += c * c;
            }
        }
        let std: Vec<f64> = var
            .iter()
            .map(|q| {
                let sd = (q / n).sqrt();
                if sd > 1e-12 { sd } else { 1.0 }
            })
            .collect();

        // Tasarım matrisi: [1, z_1..z_d], satır sıralı.
        let p = d + 1;
        let mut z = Vec::with_capacity(samples.len() * p);
        for s in samples {
            z.push(1.0);
            for ((v, m), sd) in s.x.iter().zip(&mean).zip(&std) {
                z.push((v - m) / sd);
            }
        }
        let ys: Vec<f64> = samples
            .iter()
            .map(|s| if s.y { 1.0 } else { 0.0 })
            .collect();

        let objective = |beta: &[f64]| -> f64 {
            let mut ll = 0.0f64;
            for (row, y) in z.chunks_exact(p).zip(&ys) {
                let e: f64 = row.iter().zip(beta).map(|(a, b)| a * b).sum();
                ll += y * e - softplus(e);
            }
            let pen: f64 = beta[1..].iter().map(|b| b * b).sum();
            ll - 0.5 * cfg.l2 * pen
        };

        let mut beta = vec![0.0f64; p];
        let mut obj = objective(&beta);
        let mut iterations = 0usize;
        let mut converged = false;
        while iterations < cfg.max_iter {
            iterations += 1;
            let mut g = vec![0.0f64; p];
            let mut h = vec![0.0f64; p * p];
            for (row, y) in z.chunks_exact(p).zip(&ys) {
                let e: f64 = row.iter().zip(&beta).map(|(a, b)| a * b).sum();
                let pr = sigmoid(e);
                let w = (pr * (1.0 - pr)).max(1e-12);
                let r = y - pr;
                for i in 0..p {
                    g[i] += r * row[i];
                    let wi = w * row[i];
                    for j in 0..=i {
                        h[i * p + j] += wi * row[j];
                    }
                }
            }
            for i in 1..p {
                g[i] -= cfg.l2 * beta[i];
                h[i * p + i] += cfg.l2;
            }
            // Üst üçgeni simetrikle doldur.
            for i in 0..p {
                for j in (i + 1)..p {
                    h[i * p + j] = h[j * p + i];
                }
            }
            let delta = cholesky_solve(&h, &g, p)?;
            let mut t = 1.0f64;
            let mut accepted = false;
            for _ in 0..30 {
                let cand: Vec<f64> = beta.iter().zip(&delta).map(|(b, dl)| b + t * dl).collect();
                let o = objective(&cand);
                if o.is_finite() && o >= obj {
                    beta = cand;
                    obj = o;
                    accepted = true;
                    break;
                }
                t *= 0.5;
            }
            let step = delta.iter().fold(0.0f64, |m, v| m.max((t * v).abs()));
            if !accepted || step < cfg.tol {
                // Kabul edilmeyen adım: amaç artık sayısal olarak iyileştirilemiyor (optimumda).
                converged = true;
                break;
            }
        }
        Ok(LogisticModel {
            mean,
            std,
            weights: beta[1..].to_vec(),
            bias: beta[0],
            iterations,
            converged,
        })
    }

    /// Ham (dönüştürülmüş) özellik satırı için P(y = 1).
    pub fn predict(&self, x: &[f64]) -> Option<f64> {
        if x.len() != self.weights.len() || x.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let mut e = self.bias;
        for (((v, m), sd), w) in x.iter().zip(&self.mean).zip(&self.std).zip(&self.weights) {
            e += w * (v - m) / sd;
        }
        Some(sigmoid(e))
    }
}

/// Eğitim verisinin özeti (modelle birlikte saklanır).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingSummary {
    pub n: usize,
    pub positive_rate: f64,
    pub t_start: UnixNanos,
    pub t_end: UnixNanos,
    pub skipped_no_features: usize,
    pub cv: CvReport,
}

/// Serileştirilebilir meta model: özellik adları + dönüşüm + lojistik model + eğitim özeti.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaModel {
    pub version: u32,
    pub names: Vec<String>,
    pub transform: Transform,
    pub fit: LogitConfig,
    pub model: LogisticModel,
    pub summary: TrainingSummary,
}

impl MetaModel {
    /// Ham özellik satırı + işlem yönü → P(hedef önce).
    pub fn probability(&self, side: Side, raw: &[f64]) -> Option<f64> {
        let mut x = raw.to_vec();
        self.transform.apply(side, &mut x);
        self.model.predict(&x)
    }

    pub fn to_json(&self) -> Result<String, MlError> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Sürüm denetimli okuma.
    pub fn from_json(s: &str) -> Result<Self, MlError> {
        let m: MetaModel = serde_json::from_str(s)?;
        if m.version != MODEL_VERSION {
            return Err(MlError::Version {
                found: m.version,
                expected: MODEL_VERSION,
            });
        }
        if m.names.len() != m.model.weights.len()
            || m.model.mean.len() != m.model.weights.len()
            || m.model.std.len() != m.model.weights.len()
        {
            return Err(MlError::InvalidInput("model boyutları tutarsız".into()));
        }
        Ok(m)
    }
}

/// Önce purged k-fold CV, sonra tüm veriyle son model. CV metrikleri özete yazılır.
pub fn train(ds: &Dataset, fit: &LogitConfig, cv: &CvConfig) -> Result<MetaModel, MlError> {
    let report = cross_validate(ds, cv, fit)?;
    let all: Vec<&Sample> = ds.samples.iter().collect();
    let model = LogisticModel::fit(&all, fit)?;
    Ok(MetaModel {
        version: MODEL_VERSION,
        names: ds.names.clone(),
        transform: ds.transform,
        fit: *fit,
        model,
        summary: TrainingSummary {
            n: ds.len(),
            positive_rate: ds.positive_rate(),
            t_start: ds.samples.first().map_or(UnixNanos::ZERO, |s| s.t0),
            t_end: ds
                .samples
                .iter()
                .map(|s| s.t1)
                .max()
                .unwrap_or(UnixNanos::ZERO),
            skipped_no_features: ds.skipped_no_features,
            cv: report,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: f64, y: bool) -> Sample {
        Sample {
            t0: UnixNanos(0),
            t1: UnixNanos(0),
            x: vec![x],
            y,
        }
    }

    #[test]
    fn cholesky_solves() {
        let a = [4.0, 2.0, 2.0, 3.0];
        let x = cholesky_solve(&a, &[2.0, 1.0], 2).unwrap();
        assert!((4.0 * x[0] + 2.0 * x[1] - 2.0).abs() < 1e-12);
        assert!((2.0 * x[0] + 3.0 * x[1] - 1.0).abs() < 1e-12);
        assert!(cholesky_solve(&[0.0], &[1.0], 1).is_err());
    }

    #[test]
    fn learns_direction_and_regularizes() {
        // x > 0 çoğunlukla pozitif, ama ayrılabilir değil.
        let data: Vec<Sample> = (0..200)
            .map(|i| {
                let x = (i as f64 - 100.0) / 10.0;
                let y = if i % 7 == 0 { x < 0.0 } else { x > 0.0 };
                s(x, y)
            })
            .collect();
        let refs: Vec<&Sample> = data.iter().collect();
        let m = LogisticModel::fit(&refs, &LogitConfig::default()).unwrap();
        assert!(m.converged);
        assert!(m.weights[0] > 0.0);
        assert!(m.predict(&[5.0]).unwrap() > 0.7, "{:?}", m.predict(&[5.0]));
        assert!(m.predict(&[-5.0]).unwrap() < 0.3);
        // Güçlü ceza katsayıyı küçültür.
        let strong = LogisticModel::fit(
            &refs,
            &LogitConfig {
                l2: 1e4,
                ..LogitConfig::default()
            },
        )
        .unwrap();
        assert!(strong.weights[0].abs() < m.weights[0].abs());
        assert_eq!(m.predict(&[1.0, 2.0]), None);
    }

    #[test]
    fn separable_data_stays_finite() {
        let data: Vec<Sample> = (0..50).map(|i| s(i as f64, i >= 25)).collect();
        let refs: Vec<&Sample> = data.iter().collect();
        let m = LogisticModel::fit(&refs, &LogitConfig::default()).unwrap();
        assert!(m.weights[0].is_finite() && m.weights[0] > 0.0);
    }

    #[test]
    fn rejects_bad_input() {
        let one = [s(1.0, true), s(2.0, true)];
        let refs: Vec<&Sample> = one.iter().collect();
        assert!(matches!(
            LogisticModel::fit(&refs, &LogitConfig::default()),
            Err(MlError::SingleClass)
        ));
        assert!(LogisticModel::fit(&[], &LogitConfig::default()).is_err());
        let two = [s(1.0, true), s(2.0, false)];
        let refs: Vec<&Sample> = two.iter().collect();
        let bad = LogitConfig {
            l2: 0.0,
            ..LogitConfig::default()
        };
        assert!(LogisticModel::fit(&refs, &bad).is_err());
    }
}
