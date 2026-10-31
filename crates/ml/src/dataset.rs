//! Eğitim veri kümesi: etiketli adaylar + giriş anındaki özellikler.
//!
//! Her örnek bir bilgi penceresi taşır: `t0` = giriş (karar barının kapanışı), `t1` = etiketin
//! belirlendiği barın kapanışı. Purge bu pencereyle yapılır ([`crate::cv`]).
//! Özellikler `PitColumns::row_at(t0)` ile okunur → `available_at ≤ t0` (lookahead yok).

use crate::MlError;
use crate::columns::PitColumns;
use crate::label::{Label, Outcome};
use atlas_core::{Bar, Side, UnixNanos};
use serde::{Deserialize, Serialize};

/// Ham özellik satırına yön bilgisinin nasıl katıldığı. Modelin parçasıdır (eğitimle canlı aynı).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    /// Özellikler olduğu gibi.
    Raw,
    /// Her özellik yön işaretiyle (+1 alış, −1 satış) çarpılır: "işlem yönüyle uyumlu momentum" gibi.
    SideSigned,
}

impl Transform {
    pub fn apply(self, side: Side, row: &mut [f64]) {
        if self == Transform::SideSigned {
            let s = f64::from(side.sign());
            for v in row {
                *v *= s;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Giriş anı (özellikler bu ana kadarki veriden).
    pub t0: UnixNanos,
    /// Etiketin belirlendiği an (bilgi penceresinin sonu).
    pub t1: UnixNanos,
    pub x: Vec<f64>,
    /// Meta etiketi: birincil sinyal hedefe önce ulaştı mı (+1 → `true`; −1 ve 0 → `false`).
    pub y: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dataset {
    pub names: Vec<String>,
    pub transform: Transform,
    /// `t0`'a göre sıralı.
    pub samples: Vec<Sample>,
    /// Giriş anında özelliği eksik (ısınma) olduğu için atılan etiket sayısı.
    pub skipped_no_features: usize,
}

impl Dataset {
    /// Hazır örneklerden; boyut ve sonluluk denetlenir, `t0`'a göre (kararlı) sıralanır.
    pub fn new(
        names: Vec<String>,
        transform: Transform,
        mut samples: Vec<Sample>,
    ) -> Result<Self, MlError> {
        if names.is_empty() {
            return Err(MlError::InvalidInput("en az bir özellik gerekli".into()));
        }
        for s in &samples {
            if s.x.len() != names.len() {
                return Err(MlError::InvalidInput(format!(
                    "örnek {} özellik, beklenen {}",
                    s.x.len(),
                    names.len()
                )));
            }
            if s.x.iter().any(|v| !v.is_finite()) {
                return Err(MlError::InvalidInput("sonlu olmayan özellik değeri".into()));
            }
            if s.t1 < s.t0 {
                return Err(MlError::InvalidInput("t1 < t0".into()));
            }
        }
        samples.sort_by(|a, b| a.t0.cmp(&b.t0).then(a.t1.cmp(&b.t1)));
        Ok(Dataset {
            names,
            transform,
            samples,
            skipped_no_features: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn positive_rate(&self) -> f64 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().filter(|s| s.y).count() as f64 / self.samples.len() as f64
    }
}

/// Etiketlerden veri kümesi kurar. `bars` etiketlemede kullanılan barlarla aynı olmalı.
pub fn build_dataset(
    bars: &[Bar],
    labels: &[Label],
    cols: &PitColumns,
    transform: Transform,
) -> Result<Dataset, MlError> {
    let mut samples = Vec::with_capacity(labels.len());
    let mut skipped = 0usize;
    for l in labels {
        let (Some(eb), Some(xb)) = (bars.get(l.entry_idx), bars.get(l.exit_idx)) else {
            return Err(MlError::InvalidInput(format!(
                "etiket indeksi bar dizisi dışında: {}..{}",
                l.entry_idx, l.exit_idx
            )));
        };
        let t0 = eb.close_ts;
        // Zaman noktalı okuma: available_at ≤ t0.
        let Some(mut x) = cols.row_at(t0) else {
            skipped += 1;
            continue;
        };
        transform.apply(l.side, &mut x);
        samples.push(Sample {
            t0,
            t1: xb.close_ts,
            x,
            y: l.outcome == Outcome::Profit,
        });
    }
    let mut ds = Dataset::new(cols.names().to_vec(), transform, samples)?;
    ds.skipped_no_features = skipped;
    Ok(ds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::label::{Barrier, Candidate, label_all, tests::bars};
    use std::sync::Arc;

    #[test]
    fn features_come_from_entry_time_only() {
        let b = bars(&[
            (100, 100, 100, 100),
            (100, 101, 99, 100),
            (100, 106, 99, 105),
            (105, 106, 104, 105),
        ]);
        let labels = label_all(
            &b,
            &[Candidate {
                entry_idx: 1,
                side: Side::Sell,
                barrier: Barrier {
                    profit_ticks: 5,
                    stop_ticks: 5,
                    max_bars: 2,
                },
            }],
        )
        .unwrap();
        assert_eq!(labels[0].outcome, Outcome::Stop);
        // Sütun değeri = satır indeksi; doğru satır (1) okunmalı, sonraki satır (2) asla.
        let times: Vec<UnixNanos> = b.iter().map(|x| x.close_ts).collect();
        let cols = PitColumns::new(
            vec!["i".into()],
            Arc::new(times.clone()),
            vec![Arc::new(vec![0.0, 1.0, 2.0, 3.0])],
        )
        .unwrap();
        let ds = build_dataset(&b, &labels, &cols, Transform::SideSigned).unwrap();
        assert_eq!(ds.samples[0].x, vec![-1.0]);
        assert_eq!(ds.samples[0].t0, b[1].close_ts);
        assert_eq!(ds.samples[0].t1, b[2].close_ts);
        assert!(!ds.samples[0].y);

        // Bar i'nin değeri ancak bar i+1 kapanışında kullanılabilir olsaydı (gecikmeli feature),
        // giriş anında bir önceki satır okunur.
        let mut late = times.clone();
        late.rotate_left(1);
        late[3] = late[2].plus(60_000_000_000);
        let cols = PitColumns::new(
            vec!["i".into()],
            Arc::new(late),
            vec![Arc::new(vec![0.0, 1.0, 2.0, 3.0])],
        )
        .unwrap();
        let ds = build_dataset(&b, &labels, &cols, Transform::Raw).unwrap();
        assert_eq!(ds.samples[0].x, vec![0.0]);
    }
}
