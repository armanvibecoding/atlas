//! Zaman noktalı (point-in-time) feature store (§4.2).
//!
//! - Her feature: ad, sürüm, parametre (= bağımlılık), `available_at` = bar kapanışı.
//! - `FeatureFrame` barlar geldikçe büyür (`push`); backtest önceden hesaplanmış çerçeveyi, canlı
//!   aynı `push`'u kullanır → aynı kod yolu (kural 3).
//! - `FeatureStore` hesabı bir kez yapar, (veri versiyonu, feature) anahtarıyla önbelleğe yazar.
//! - `lookahead::check` her feature'ı gelecek verisi bozulmuş seriyle yeniden hesaplar; geçmiş
//!   değerler değişirse feature geleceğe bakıyordur (kural 4).

pub mod calc;
pub mod lookahead;
pub mod spec;

pub use spec::{FEATURE_VERSION, FeatureSpec, Kind};

use atlas_core::{Bar, SessionSpec, UnixNanos};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Sütunlu feature değerleri. `NaN` = henüz yok (ısınma).
pub struct FeatureFrame {
    specs: Vec<FeatureSpec>,
    calcs: Vec<Box<dyn calc::Calc>>,
    cols: Vec<Vec<f64>>,
    available_at: Vec<UnixNanos>,
    session: SessionSpec,
}

impl FeatureFrame {
    pub fn new(specs: &[FeatureSpec], session: SessionSpec) -> Self {
        let mut uniq: Vec<FeatureSpec> = specs.to_vec();
        uniq.sort();
        uniq.dedup();
        FeatureFrame {
            calcs: uniq.iter().map(calc::make).collect(),
            cols: vec![Vec::new(); uniq.len()],
            specs: uniq,
            available_at: Vec::new(),
            session,
        }
    }

    pub fn compute(specs: &[FeatureSpec], session: SessionSpec, bars: &[Bar]) -> Self {
        let mut f = Self::new(specs, session);
        for c in &mut f.cols {
            c.reserve(bars.len());
        }
        for b in bars {
            f.push(b);
        }
        f
    }

    /// Bir bar ekler; tüm feature'lar yalnız bu bar dahil geçmişi görerek güncellenir.
    pub fn push(&mut self, b: &Bar) {
        let day = calc::day_of(&self.session, b);
        for (c, col) in self.calcs.iter_mut().zip(self.cols.iter_mut()) {
            col.push(c.update(b, day).unwrap_or(f64::NAN));
        }
        self.available_at.push(b.close_ts);
    }

    pub fn len(&self) -> usize {
        self.available_at.len()
    }

    pub fn is_empty(&self) -> bool {
        self.available_at.is_empty()
    }

    pub fn specs(&self) -> &[FeatureSpec] {
        &self.specs
    }

    pub fn index_of(&self, s: &FeatureSpec) -> Option<usize> {
        self.specs.binary_search(s).ok()
    }

    pub fn column(&self, s: &FeatureSpec) -> Option<&[f64]> {
        self.index_of(s).map(|i| self.cols[i].as_slice())
    }

    /// `i`. barın değeri; `now` anında henüz kullanılabilir değilse `None` (zaman noktalı erişim).
    pub fn value_at(&self, col: usize, i: usize, now: UnixNanos) -> Option<f64> {
        let at = *self.available_at.get(i)?;
        if at > now {
            return None;
        }
        let v = *self.cols.get(col)?.get(i)?;
        (!v.is_nan()).then_some(v)
    }

    pub fn available_at(&self) -> &[UnixNanos] {
        &self.available_at
    }
}

/// Bir enstrümanın barları için hesaplanmış, paylaşılan sütunlar.
#[derive(Clone)]
pub struct SharedColumns {
    pub available_at: Arc<Vec<UnixNanos>>,
    pub cols: BTreeMap<FeatureSpec, Arc<Vec<f64>>>,
}

/// Önbellek anahtarı: (veri versiyonu, feature, feature sürümü).
type CacheKey = (String, FeatureSpec, u32);

/// (veri versiyonu, feature) → sütun önbelleği. Aynı veri üzerinde binlerce genom aynı sütunları paylaşır.
#[derive(Default)]
pub struct FeatureStore {
    cache: Mutex<BTreeMap<CacheKey, Arc<Vec<f64>>>>,
    times: Mutex<BTreeMap<String, Arc<Vec<UnixNanos>>>>,
    computed: Mutex<u64>,
}

impl FeatureStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Kaç feature sütunu gerçekten hesaplandı (önbellek isabetleri hariç).
    pub fn computed_count(&self) -> u64 {
        self.computed.lock().map(|g| *g).unwrap_or(0)
    }

    pub fn columns(
        &self,
        data_version: &str,
        session: &SessionSpec,
        bars: &[Bar],
        specs: &[FeatureSpec],
    ) -> SharedColumns {
        let mut missing: Vec<FeatureSpec> = Vec::new();
        let mut out: BTreeMap<FeatureSpec, Arc<Vec<f64>>> = BTreeMap::new();
        if let Ok(c) = self.cache.lock() {
            for s in specs {
                match c.get(&(data_version.to_string(), *s, FEATURE_VERSION)) {
                    Some(v) => {
                        out.insert(*s, v.clone());
                    }
                    None => missing.push(*s),
                }
            }
        } else {
            missing.extend_from_slice(specs);
        }
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            let f = FeatureFrame::compute(&missing, session.clone(), bars);
            if let Ok(mut c) = self.cache.lock() {
                for (s, col) in f.specs.iter().zip(f.cols) {
                    let a = Arc::new(col);
                    c.insert((data_version.to_string(), *s, FEATURE_VERSION), a.clone());
                    out.insert(*s, a);
                }
            }
            if let Ok(mut n) = self.computed.lock() {
                *n += missing.len() as u64;
            }
        }
        let times = {
            let mut t = match self.times.lock() {
                Ok(t) => t,
                Err(p) => p.into_inner(),
            };
            t.entry(data_version.to_string())
                .or_insert_with(|| Arc::new(bars.iter().map(|b| b.close_ts).collect()))
                .clone()
        };
        SharedColumns {
            available_at: times,
            cols: out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::Price;

    pub(crate) fn bars(n: usize) -> Vec<Bar> {
        // Deterministik, dalgalı seri: hem trend hem salınım.
        (0..n)
            .map(|i| {
                let t = i as i64;
                let base = 10_000 + (t / 3) % 50 + ((t * 7919) % 23) - 11 + t / 10;
                let o = base;
                let c = base + ((t * 31) % 9) - 4;
                let h = o.max(c) + (t % 5);
                let l = o.min(c) - ((t * 3) % 4);
                let open_ts = UnixNanos(1_700_000_000_000_000_000 + i as u64 * 60_000_000_000);
                Bar {
                    open_ts,
                    close_ts: open_ts.plus(60_000_000_000),
                    open: Price(o),
                    high: Price(h),
                    low: Price(l),
                    close: Price(c),
                    volume: 10 + (i as u64 * 13) % 50,
                    buy_volume: 5 + (i as u64 * 7) % 10,
                    trades: 5,
                }
            })
            .collect()
    }

    fn all_specs() -> Vec<FeatureSpec> {
        Kind::ALL
            .iter()
            .map(|k| FeatureSpec::new(*k, k.range().2))
            .collect()
    }

    #[test]
    fn every_feature_produces_values_after_warmup() {
        let b = bars(5000);
        let f = FeatureFrame::compute(&all_specs(), SessionSpec::Always, &b);
        for s in f.specs() {
            let col = f.column(s).unwrap();
            assert_eq!(col.len(), b.len());
            assert!(col.iter().any(|v| !v.is_nan()), "{s} hiç değer üretmedi");
        }
        // EMA ısınması: ilk 19 bar NaN, 20. bar değer
        let ema = f.column(&"ema:20".parse().unwrap()).unwrap();
        assert!(ema[18].is_nan() && !ema[19].is_nan());
    }

    #[test]
    fn value_at_respects_availability() {
        let b = bars(50);
        let f = FeatureFrame::compute(&["close".parse().unwrap()], SessionSpec::Always, &b);
        let col = f.index_of(&"close".parse().unwrap()).unwrap();
        // Bar 10'un değeri kapanışından önce okunamaz.
        assert_eq!(f.value_at(col, 10, b[10].close_ts.minus(1)), None);
        assert_eq!(
            f.value_at(col, 10, b[10].close_ts),
            Some(b[10].close.0 as f64)
        );
    }

    #[test]
    fn store_caches_by_version() {
        let b = bars(2000);
        let st = FeatureStore::new();
        let specs: Vec<FeatureSpec> = vec!["ema:20".parse().unwrap(), "atr:14".parse().unwrap()];
        let a = st.columns("v1", &SessionSpec::Always, &b, &specs);
        assert_eq!(st.computed_count(), 2);
        let a2 = st.columns("v1", &SessionSpec::Always, &b, &specs);
        assert_eq!(st.computed_count(), 2);
        assert!(Arc::ptr_eq(&a.cols[&specs[0]], &a2.cols[&specs[0]]));
        st.columns("v2", &SessionSpec::Always, &b, &specs[..1]);
        assert_eq!(st.computed_count(), 3);
    }
}
