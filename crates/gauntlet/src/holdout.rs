//! Kilitli holdout (kural 8). Holdout barlarına **yalnız** `HoldoutVault::access` ile erişilir;
//! her erişim sicile yazılır ve bütçeden düşer. Bütçe bitince terfi yok.

use atlas_core::units::NANOS_PER_DAY;
use atlas_core::{Bar, UnixNanos};
use atlas_registry::{Registry, RegistryError};

pub struct HoldoutVault {
    dataset: String,
    bars: Vec<Bar>,
    from: UnixNanos,
    to: UnixNanos,
    budget: u64,
}

pub struct HoldoutAccess<'a> {
    pub bars: &'a [Bar],
    pub from: UnixNanos,
    pub used: u64,
    pub budget: u64,
}

/// Son `days` günü kilitler. (in-sample barlar, kasa).
pub fn split(dataset: &str, bars: Vec<Bar>, days: u64, budget: u64) -> (Vec<Bar>, HoldoutVault) {
    let last = bars.last().map_or(UnixNanos::ZERO, |b| b.close_ts);
    let from = last.minus(days * NANOS_PER_DAY);
    let cut = bars.partition_point(|b| b.open_ts < from);
    let mut is = bars;
    let hold = is.split_off(cut);
    let from = hold.first().map_or(from, |b| b.open_ts);
    (
        is,
        HoldoutVault {
            dataset: dataset.to_string(),
            bars: hold,
            from,
            to: last,
            budget,
        },
    )
}

impl HoldoutVault {
    pub fn period(&self) -> (UnixNanos, UnixNanos) {
        (self.from, self.to)
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// Erişimi sicile kaydeder (bütçe kontrolü) ve barları verir.
    pub fn access(
        &self,
        reg: &mut Registry,
        genome_hash: &str,
        now: UnixNanos,
    ) -> Result<HoldoutAccess<'_>, RegistryError> {
        let id = reg.ensure_holdout(&self.dataset, self.from.0, self.to.0, self.budget, now.0)?;
        let (used, budget) = reg.record_holdout_access(id, genome_hash, now.0)?;
        Ok(HoldoutAccess {
            bars: &self.bars,
            from: self.from,
            used,
            budget,
        })
    }
}
