//! Purged + embargo'lu k-fold çapraz doğrulama (López de Prado, AFML §7.4).
//!
//! - Örnekler `t0`'a göre sıralıdır; katlar ardışık bloklardır (karıştırma yok).
//! - Test aralığı `[ilk t0, en geç t1]` (test örneklerinin bilgi pencerelerinin birleşimi).
//! - **Purge:** bilgi penceresi `[t0, t1]` test aralığıyla kesişen her eğitim adayı atılır (testten
//!   önce başlayıp test içine taşanlar dahil).
//! - **Embargo:** test aralığının bitişinden sonraki `embargo_ns` içinde başlayan eğitim adayları da
//!   atılır (testten hemen sonraki seri korelasyonlu örnekler).
//! - Metrikler kat dışı (out-of-fold) tahminlerle: havuzlanmış AUC/log-loss + kat başı değerler.

use crate::MlError;
use crate::dataset::{Dataset, Sample};
use crate::metrics::{auc, log_loss};
use crate::model::{LogisticModel, LogitConfig};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CvConfig {
    /// Kat sayısı (≥ 2).
    pub k: usize,
    /// Test aralığından sonraki embargo süresi (nanosaniye).
    pub embargo_ns: u64,
}

impl Default for CvConfig {
    fn default() -> Self {
        CvConfig {
            k: 5,
            embargo_ns: 0,
        }
    }
}

/// Bir katın indeksleri (veri kümesindeki örnek sırasına göre).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub fold: usize,
    pub train: Vec<usize>,
    pub test: Vec<usize>,
    pub purged: Vec<usize>,
    pub embargoed: Vec<usize>,
}

/// Kat bölmelerini üretir. Örnekler `t0`'a göre sıralı olmalı.
pub fn purged_kfold_splits(samples: &[Sample], cfg: &CvConfig) -> Result<Vec<Split>, MlError> {
    let n = samples.len();
    if cfg.k < 2 || n < cfg.k {
        return Err(MlError::InvalidInput(format!(
            "k = {} (≥ 2 ve ≤ örnek sayısı {n} olmalı)",
            cfg.k
        )));
    }
    if samples.windows(2).any(|w| w[1].t0 < w[0].t0) {
        return Err(MlError::InvalidInput(
            "örnekler t0'a göre sıralı değil".into(),
        ));
    }
    let mut out = Vec::with_capacity(cfg.k);
    for f in 0..cfg.k {
        let lo = f * n / cfg.k;
        let hi = (f + 1) * n / cfg.k;
        let test: Vec<usize> = (lo..hi).collect();
        let start = samples[lo].t0;
        let end = samples[lo..hi].iter().map(|s| s.t1).max().unwrap_or(start);
        let embargo_end = end.plus(cfg.embargo_ns);
        let (mut train, mut purged, mut embargoed) = (Vec::new(), Vec::new(), Vec::new());
        for (j, s) in samples.iter().enumerate() {
            if (lo..hi).contains(&j) {
                continue;
            }
            if s.t0 <= end && s.t1 >= start {
                purged.push(j);
            } else if s.t0 > end && s.t0 <= embargo_end {
                embargoed.push(j);
            } else {
                train.push(j);
            }
        }
        out.push(Split {
            fold: f,
            train,
            test,
            purged,
            embargoed,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldMetrics {
    pub fold: usize,
    pub n_train: usize,
    pub n_test: usize,
    pub n_purged: usize,
    pub n_embargoed: usize,
    pub test_positive_rate: f64,
    /// Eğitim kümesi tek sınıfsa kat değerlendirilmez (`None`).
    pub auc: Option<f64>,
    pub log_loss: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CvReport {
    pub k: usize,
    pub embargo_ns: u64,
    pub folds: Vec<FoldMetrics>,
    /// Havuzlanmış kat dışı AUC.
    pub auc: Option<f64>,
    /// Havuzlanmış kat dışı log-loss.
    pub log_loss: Option<f64>,
    /// Kat AUC'lerinin ortalaması (değerlendirilen katlar).
    pub mean_fold_auc: Option<f64>,
    /// Kat dışı tahmini olan örnek sayısı.
    pub n_oof: usize,
}

/// Purged k-fold CV; her katta model yalnız eğitim indeksleriyle eğitilir.
pub fn cross_validate(ds: &Dataset, cv: &CvConfig, fit: &LogitConfig) -> Result<CvReport, MlError> {
    let splits = purged_kfold_splits(&ds.samples, cv)?;
    let mut folds = Vec::with_capacity(splits.len());
    let mut oof_p = Vec::new();
    let mut oof_y = Vec::new();
    for sp in &splits {
        let train: Vec<&Sample> = sp.train.iter().map(|&i| &ds.samples[i]).collect();
        let test_y: Vec<bool> = sp.test.iter().map(|&i| ds.samples[i].y).collect();
        let pos = test_y.iter().filter(|y| **y).count() as f64 / test_y.len().max(1) as f64;
        let (fa, fl) = match LogisticModel::fit(&train, fit) {
            Ok(m) => {
                let ps: Vec<f64> = sp
                    .test
                    .iter()
                    .map(|&i| m.predict(&ds.samples[i].x).unwrap_or(0.5))
                    .collect();
                let r = (auc(&ps, &test_y), log_loss(&ps, &test_y));
                oof_p.extend_from_slice(&ps);
                oof_y.extend_from_slice(&test_y);
                r
            }
            Err(MlError::SingleClass) | Err(MlError::InvalidInput(_)) => (None, None),
            Err(e) => return Err(e),
        };
        folds.push(FoldMetrics {
            fold: sp.fold,
            n_train: sp.train.len(),
            n_test: sp.test.len(),
            n_purged: sp.purged.len(),
            n_embargoed: sp.embargoed.len(),
            test_positive_rate: pos,
            auc: fa,
            log_loss: fl,
        });
    }
    let fold_aucs: Vec<f64> = folds.iter().filter_map(|f| f.auc).collect();
    Ok(CvReport {
        k: cv.k,
        embargo_ns: cv.embargo_ns,
        auc: auc(&oof_p, &oof_y),
        log_loss: log_loss(&oof_p, &oof_y),
        mean_fold_auc: (!fold_aucs.is_empty())
            .then(|| fold_aucs.iter().sum::<f64>() / fold_aucs.len() as f64),
        n_oof: oof_p.len(),
        folds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::UnixNanos;

    fn samples(n: u64, step: u64, width: u64) -> Vec<Sample> {
        (0..n)
            .map(|i| Sample {
                t0: UnixNanos(i * step),
                t1: UnixNanos(i * step + width),
                x: vec![i as f64],
                y: i % 2 == 0,
            })
            .collect()
    }

    #[test]
    fn purge_and_embargo_drop_overlapping_samples() {
        // 10 örnek, t0 = 10i, t1 = t0 + 25 → her örnek sonraki iki örnekle örtüşür.
        let s = samples(10, 10, 25);
        let sp = purged_kfold_splits(
            &s,
            &CvConfig {
                k: 5,
                embargo_ns: 10,
            },
        )
        .unwrap();
        // Kat 0: test {0,1}, aralık [0, 35]. 2 (20..45) ve 3 (30..55) örtüşür → purge.
        // 4 (t0 = 40) embargo (35, 45] içinde.
        assert_eq!(sp[0].test, vec![0, 1]);
        assert_eq!(sp[0].purged, vec![2, 3]);
        assert_eq!(sp[0].embargoed, vec![4]);
        assert_eq!(sp[0].train, vec![5, 6, 7, 8, 9]);
        // Kat 2: test {4,5}, aralık [40, 75]. Önce: 2 (t1 45) ve 3 (t1 55) içeri taşar → purge;
        // 1 (t1 35) temiz. Sonra: 6 (60), 7 (70) purge; 8 (80) embargo (75, 85]; 9 eğitim.
        assert_eq!(sp[2].purged, vec![2, 3, 6, 7]);
        assert_eq!(sp[2].embargoed, vec![8]);
        assert_eq!(sp[2].train, vec![0, 1, 9]);
    }

    #[test]
    fn no_train_sample_overlaps_test_window() {
        // Değişken genişlikli pencereler, tüm katlarda değişmez kontrol.
        let s: Vec<Sample> = (0..300u64)
            .map(|i| Sample {
                t0: UnixNanos(i * 7),
                t1: UnixNanos(i * 7 + (i * 37) % 60),
                x: vec![0.0],
                y: i % 3 == 0,
            })
            .collect();
        let cfg = CvConfig {
            k: 6,
            embargo_ns: 20,
        };
        for sp in purged_kfold_splits(&s, &cfg).unwrap() {
            let start = s[sp.test[0]].t0;
            let end = sp.test.iter().map(|&i| s[i].t1).max().unwrap();
            for &j in &sp.train {
                let overlaps = s[j].t0 <= end && s[j].t1 >= start;
                let in_embargo = s[j].t0 > end && s[j].t0 <= end.plus(cfg.embargo_ns);
                assert!(!overlaps && !in_embargo, "kat {} örnek {j}", sp.fold);
            }
            assert_eq!(
                sp.train.len() + sp.test.len() + sp.purged.len() + sp.embargoed.len(),
                s.len()
            );
        }
    }

    #[test]
    fn rejects_bad_config() {
        let s = samples(3, 10, 5);
        assert!(
            purged_kfold_splits(
                &s,
                &CvConfig {
                    k: 1,
                    embargo_ns: 0
                }
            )
            .is_err()
        );
        assert!(
            purged_kfold_splits(
                &s,
                &CvConfig {
                    k: 4,
                    embargo_ns: 0
                }
            )
            .is_err()
        );
        let mut r = s.clone();
        r.swap(0, 2);
        assert!(
            purged_kfold_splits(
                &r,
                &CvConfig {
                    k: 2,
                    embargo_ns: 0
                }
            )
            .is_err()
        );
    }
}
