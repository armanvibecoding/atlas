//! Veri katmanı: kaynaklar, yerel depo, versiyonlama, kalite monitörü (§4.1).

pub mod binance;
pub mod databento;
pub mod quality;
pub mod store;

use atlas_core::{InstrumentId, MarketEvent, UnixNanos};
pub use store::{DataVersion, Store, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub from: UnixNanos,
    pub to: UnixNanos,
}

/// Backtest ve canlı aynı tipte olay üretir (kural 3).
pub trait DataSource {
    fn events(&mut self, range: TimeRange)
    -> Result<impl Iterator<Item = MarketEvent>, StoreError>;
    /// Son yüklenen verinin içerik versiyonu (deney kaydı bunu taşır).
    fn version(&self) -> Option<&DataVersion>;
}

/// Yerel depodan bar kaynağı; birden çok enstrümanı zaman sırasında birleştirir.
pub struct StoreSource {
    store: Store,
    items: Vec<(InstrumentId, String, String, String)>,
    version: Option<DataVersion>,
}

impl StoreSource {
    pub fn new(store: Store) -> Self {
        StoreSource {
            store,
            items: Vec::new(),
            version: None,
        }
    }

    pub fn with(mut self, inst: InstrumentId, source: &str, symbol: &str, schema: &str) -> Self {
        self.items
            .push((inst, source.into(), symbol.into(), schema.into()));
        self
    }
}

impl DataSource for StoreSource {
    fn events(
        &mut self,
        range: TimeRange,
    ) -> Result<impl Iterator<Item = MarketEvent>, StoreError> {
        let mut all = Vec::new();
        let mut versions = Vec::new();
        for (inst, source, symbol, schema) in &self.items {
            let (bars, v) = self
                .store
                .load_bars(source, symbol, schema, range.from, range.to)?;
            versions.push(v);
            all.extend(store::bars_to_events(*inst, &bars));
        }
        // Zaman, sonra enstrüman kimliği: deterministik birleştirme.
        all.sort_by_key(|e| (e.ts, e.instrument));
        self.version = Some(DataVersion::combine(&versions));
        Ok(all.into_iter())
    }

    fn version(&self) -> Option<&DataVersion> {
        self.version.as_ref()
    }
}
