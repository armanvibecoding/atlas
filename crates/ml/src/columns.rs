//! Zaman noktalı (point-in-time) özellik sütunları.
//!
//! Her satır bir `available_at` damgası taşır. [`PitColumns::row_at`] `now` anında **kullanılabilir
//! olan son satırı** verir (`available_at ≤ now`); damgası gelecekte olan satır hiçbir yoldan
//! okunamaz (kural 4). Eğitim (giriş anı = karar barının kapanışı) ve canlı süzme
//! ([`crate::MetaFiltered`], `ctx.now()`) aynı fonksiyonu kullanır.

use crate::MlError;
use atlas_core::UnixNanos;
use atlas_features::{FeatureSpec, SharedColumns};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PitColumns {
    names: Vec<String>,
    available_at: Arc<Vec<UnixNanos>>,
    cols: Vec<Arc<Vec<f64>>>,
}

impl PitColumns {
    /// Elle verilmiş sütunlar. Uzunluklar eşit, `available_at` azalmayan olmalı.
    pub fn new(
        names: Vec<String>,
        available_at: Arc<Vec<UnixNanos>>,
        cols: Vec<Arc<Vec<f64>>>,
    ) -> Result<Self, MlError> {
        if names.len() != cols.len() {
            return Err(MlError::InvalidInput(format!(
                "{} ad, {} sütun",
                names.len(),
                cols.len()
            )));
        }
        if names.is_empty() {
            return Err(MlError::InvalidInput("en az bir özellik gerekli".into()));
        }
        if let Some((n, c)) = names
            .iter()
            .zip(&cols)
            .find(|(_, c)| c.len() != available_at.len())
        {
            return Err(MlError::InvalidInput(format!(
                "{n}: {} değer, {} zaman damgası",
                c.len(),
                available_at.len()
            )));
        }
        if available_at.windows(2).any(|w| w[1] < w[0]) {
            return Err(MlError::InvalidInput(
                "available_at azalmayan sırada olmalı".into(),
            ));
        }
        Ok(PitColumns {
            names,
            available_at,
            cols,
        })
    }

    /// `FeatureStore`'un paylaşılan sütunlarından, verilen sırayla. Ad = feature'ın metin biçimi.
    pub fn from_shared(shared: &SharedColumns, specs: &[FeatureSpec]) -> Result<Self, MlError> {
        let cols = specs
            .iter()
            .map(|s| {
                shared
                    .cols
                    .get(s)
                    .cloned()
                    .ok_or_else(|| MlError::InvalidInput(format!("{s} sütunu hesaplanmamış")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(
            specs.iter().map(ToString::to_string).collect(),
            shared.available_at.clone(),
            cols,
        )
    }

    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn len(&self) -> usize {
        self.available_at.len()
    }

    pub fn is_empty(&self) -> bool {
        self.available_at.is_empty()
    }

    /// `now` anında kullanılabilir son satırın indeksi.
    pub fn row_index_at(&self, now: UnixNanos) -> Option<usize> {
        last_available(&self.available_at, now)
    }

    /// `now` anında kullanılabilir son satır. Değerlerden biri yoksa (`NaN`, ısınma) `None`.
    pub fn row_at(&self, now: UnixNanos) -> Option<Vec<f64>> {
        let i = self.row_index_at(now)?;
        self.cols
            .iter()
            .map(|c| c.get(i).copied().filter(|v| v.is_finite()))
            .collect()
    }
}

/// `available_at ≤ now` olan son indeks (dizi azalmayan sırada).
pub(crate) fn last_available(available_at: &[UnixNanos], now: UnixNanos) -> Option<usize> {
    available_at.partition_point(|t| *t <= now).checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> PitColumns {
        PitColumns::new(
            vec!["a".into(), "b".into()],
            Arc::new(vec![UnixNanos(10), UnixNanos(20), UnixNanos(30)]),
            vec![
                Arc::new(vec![1.0, 2.0, 3.0]),
                Arc::new(vec![f64::NAN, 20.0, 30.0]),
            ],
        )
        .unwrap()
    }

    #[test]
    fn row_at_never_reads_future() {
        let c = cols();
        assert_eq!(c.row_at(UnixNanos(9)), None);
        // Satır 0'da b henüz ısınmada.
        assert_eq!(c.row_at(UnixNanos(10)), None);
        assert_eq!(c.row_at(UnixNanos(29)), Some(vec![2.0, 20.0]));
        assert_eq!(c.row_at(UnixNanos(30)), Some(vec![3.0, 30.0]));
        assert_eq!(c.row_at(UnixNanos(1_000)), Some(vec![3.0, 30.0]));
    }

    #[test]
    fn validates_shapes() {
        let t = Arc::new(vec![UnixNanos(1), UnixNanos(2)]);
        assert!(PitColumns::new(vec!["a".into()], t.clone(), vec![Arc::new(vec![1.0])]).is_err());
        assert!(PitColumns::new(vec![], t.clone(), vec![]).is_err());
        let back = Arc::new(vec![UnixNanos(2), UnixNanos(1)]);
        assert!(PitColumns::new(vec!["a".into()], back, vec![Arc::new(vec![1.0, 2.0])]).is_err());
    }
}
