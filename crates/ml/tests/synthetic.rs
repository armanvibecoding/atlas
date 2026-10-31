#![allow(clippy::unwrap_used)]
//! Sentetik veride uçtan uca meta-labeling: etiketle → zaman noktalı özellik → purged CV → model.
//!
//! - Tahmin gücü olan özellik: AR(1) momentumlu barlarda son getirinin yönle çarpımı → CV AUC > 0.65.
//! - Saf gürültü özellikler → |AUC − 0.5| < 0.05.
//! - Determinizm: aynı veri → bit düzeyinde aynı katsayılar ve JSON.

use atlas_core::{Bar, Price, Side, UnixNanos};
use atlas_ml::{
    Barrier, Candidate, CvConfig, Dataset, LogitConfig, MetaModel, PitColumns, Transform,
    build_dataset, label_all, train,
};
use atlas_sim::synthetic::ar_bars;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::sync::Arc;

const N_BARS: usize = 60_000;
const EVERY: usize = 10;

fn bars(seed: u64, phi: f64) -> Vec<Bar> {
    ar_bars(
        seed,
        N_BARS,
        UnixNanos(1_700_000_000_000_000_000),
        phi,
        4.0,
        Price(100_000),
    )
}

/// Her `EVERY` barda bir aday; yön tohumlu ChaCha ile rastgele (birincil sinyal "yazı-tura").
fn candidates(seed: u64, n: usize) -> Vec<Candidate> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (20..n.saturating_sub(1))
        .step_by(EVERY)
        .map(|i| Candidate {
            entry_idx: i,
            side: if rng.random_bool(0.5) {
                Side::Buy
            } else {
                Side::Sell
            },
            barrier: Barrier {
                profit_ticks: 8,
                stop_ticks: 8,
                max_bars: 20,
            },
        })
        .collect()
}

fn times(b: &[Bar]) -> Arc<Vec<UnixNanos>> {
    Arc::new(b.iter().map(|x| x.close_ts).collect())
}

/// Son 1 ve 3 barın getirisi (tick); bar i'nin değeri i. barın kapanışında kullanılabilir.
fn momentum_cols(b: &[Bar]) -> PitColumns {
    let ret = |k: usize| -> Arc<Vec<f64>> {
        Arc::new(
            (0..b.len())
                .map(|i| {
                    if i >= k {
                        (b[i].close - b[i - k].close) as f64
                    } else {
                        f64::NAN
                    }
                })
                .collect(),
        )
    };
    PitColumns::new(
        vec!["ret1".into(), "ret3".into()],
        times(b),
        vec![ret(1), ret(3)],
    )
    .unwrap()
}

/// Fiyattan bağımsız saf gürültü sütunları.
fn noise_cols(b: &[Bar], seed: u64) -> PitColumns {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut col = || -> Arc<Vec<f64>> {
        Arc::new(
            (0..b.len())
                .map(|_| (0..12).map(|_| rng.random::<f64>()).sum::<f64>() - 6.0)
                .collect(),
        )
    };
    let cols = vec![col(), col(), col()];
    PitColumns::new(vec!["n1".into(), "n2".into(), "n3".into()], times(b), cols).unwrap()
}

fn cv() -> CvConfig {
    CvConfig {
        k: 5,
        // 20 bar (zaman bariyeri) kadar embargo.
        embargo_ns: 20 * 60_000_000_000,
    }
}

fn dataset(b: &[Bar], cols: &PitColumns, seed: u64) -> Dataset {
    let labels = label_all(b, &candidates(seed, b.len())).unwrap();
    build_dataset(b, &labels, cols, Transform::SideSigned).unwrap()
}

#[test]
fn predictive_feature_has_cv_auc_above_065() {
    let b = bars(11, 0.5);
    let ds = dataset(&b, &momentum_cols(&b), 3);
    assert!(ds.len() > 5_000, "örnek sayısı {}", ds.len());
    let m = train(&ds, &LogitConfig::default(), &cv()).unwrap();
    let auc = m.summary.cv.auc.unwrap();
    eprintln!(
        "tahmin gücü olan özellik: n = {}, pozitif oranı = {:.3}, CV AUC = {auc:.4}, kat AUC = {:?}, log-loss = {:.4}",
        m.summary.n,
        m.summary.positive_rate,
        m.summary
            .cv
            .folds
            .iter()
            .map(|f| f.auc.map(|a| (a * 1000.0).round() / 1000.0))
            .collect::<Vec<_>>(),
        m.summary.cv.log_loss.unwrap()
    );
    assert!(auc > 0.65, "CV AUC {auc}");
    for f in &m.summary.cv.folds {
        assert!(f.auc.unwrap() > 0.6, "kat {} AUC {:?}", f.fold, f.auc);
    }
    // Kat sınırlarında örtüşen/yakın örnekler eğitimden çıkarılmış olmalı.
    let removed: usize = m
        .summary
        .cv
        .folds
        .iter()
        .map(|f| f.n_purged + f.n_embargoed)
        .sum();
    assert!(removed > 0);
    // Momentum yönle uyumluysa hedef olasılığı artar.
    assert!(m.model.weights.iter().any(|w| *w > 0.0));
    assert!(m.model.converged);
}

#[test]
fn pure_noise_has_auc_near_half() {
    let b = bars(12, 0.0);
    let ds = dataset(&b, &noise_cols(&b, 99), 4);
    let m = train(&ds, &LogitConfig::default(), &cv()).unwrap();
    let auc = m.summary.cv.auc.unwrap();
    eprintln!(
        "saf gürültü: n = {}, pozitif oranı = {:.3}, CV AUC = {auc:.4}",
        m.summary.n, m.summary.positive_rate
    );
    assert!((auc - 0.5).abs() < 0.05, "gürültü AUC {auc}");
}

#[test]
fn training_is_bit_deterministic_and_roundtrips() {
    let b = bars(13, 0.3);
    let cols = momentum_cols(&b);
    let a = train(&dataset(&b, &cols, 5), &LogitConfig::default(), &cv()).unwrap();
    let c = train(&dataset(&b, &cols, 5), &LogitConfig::default(), &cv()).unwrap();
    let bits = |m: &MetaModel| -> Vec<u64> {
        m.model
            .weights
            .iter()
            .chain([&m.model.bias])
            .chain(&m.model.mean)
            .chain(&m.model.std)
            .map(|v| v.to_bits())
            .collect()
    };
    assert_eq!(bits(&a), bits(&c));
    let ja = a.to_json().unwrap();
    assert_eq!(ja, c.to_json().unwrap());

    // Serileştirme: geri okunan model aynı olasılıkları verir; sürüm denetlenir.
    let back = MetaModel::from_json(&ja).unwrap();
    assert_eq!(bits(&back), bits(&a));
    assert_eq!(back.names, a.names);
    assert_eq!(back.transform, a.transform);
    assert_eq!(back.summary.n, a.summary.n);
    let row = cols.row_at(b[500].close_ts).unwrap();
    assert_eq!(
        back.probability(Side::Sell, &row).map(f64::to_bits),
        a.probability(Side::Sell, &row).map(f64::to_bits)
    );
    let bumped = ja.replacen("\"version\": 1", "\"version\": 99", 1);
    assert!(matches!(
        MetaModel::from_json(&bumped),
        Err(atlas_ml::MlError::Version { found: 99, .. })
    ));
    // Özet: n, pozitif oranı, CV metrikleri.
    assert_eq!(a.summary.n, a.summary.cv.n_oof);
    assert!(a.summary.positive_rate > 0.0 && a.summary.positive_rate < 1.0);
    assert!(a.summary.cv.auc.is_some() && a.summary.cv.log_loss.is_some());
}
