//! Dolum kalibrasyonu (§4.4, M4 kabulü).
//!
//! Paper/canlı dolumlar simülasyondaki karşılıklarıyla eşlenir (`FillObservation`); kayma ve limit
//! dolum olasılığı ölçülür (`measure`) ve parametreler güncellenir (`calibrate`). Her kalibrasyon yeni
//! bir **sürüm** üretir; sürümler `CalibrationStore` dizinine (varsayılan `var/calibration/`) ayrı
//! dosyalar olarak yazılır, eskisi asla üzerine yazılmaz. Deney kaydı `FillParams::id()`
//! (ör. `queue@v3`) taşır; yeni sürüm geçmiş sonuçları geçersiz kılmaz.
//!
//! Fiyatlar tick (`Price`), kayma parametreleri tamsayı tick; f64 yalnız istatistik ve olasılıkta.

use crate::fill_queue::{QUEUE_MODEL_NAME, impact_ticks};
use atlas_core::{Price, Side};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Kalibrasyon dosyalarının varsayılan dizini (çalışma dizinine göre).
pub const DEFAULT_CALIBRATION_DIR: &str = "var/calibration";

/// Eşlenen emrin türü.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservedKind {
    Market,
    Stop,
    Limit,
}

/// Bir paper/canlı emir ile simülasyondaki karşılığı.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillObservation {
    /// Enstrüman sembolü (`Instrument::symbol`).
    pub instrument: String,
    pub side: Side,
    pub kind: ObservedKind,
    /// Lot.
    pub qty: i32,
    /// Kaymasız referans: piyasa/stop için tetik anındaki işlem fiyatı, limit için limit fiyatı.
    pub reference_price: Price,
    /// Simülasyonun dolum fiyatı.
    pub sim_price: Price,
    /// Paper/canlı dolum fiyatı; `filled == false` ise anlamsızdır.
    pub live_price: Price,
    /// Paper/canlı emir doldu mu? (Limit gözlemleri simülasyonun "doldu" dediği emirlerdir.)
    pub filled: bool,
}

/// Fiyat farkı istatistikleri (tick). Pozitif kayma = aleyhte.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PriceDiffStats {
    /// Dolmuş piyasa/stop gözlemi sayısı.
    pub n: usize,
    pub live_mean_adverse_ticks: f64,
    pub live_median_adverse_ticks: f64,
    pub sim_mean_adverse_ticks: f64,
    pub sim_median_adverse_ticks: f64,
    /// Ortalama (sim − canlı) aleyhte kayma; pozitif = simülasyon karamsar.
    pub mean_sim_minus_live_ticks: f64,
    /// Ortalama |sim − canlı| dolum fiyatı farkı. M4 kabul ölçüsü.
    pub mean_abs_sim_live_diff_ticks: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SlippageStats {
    /// Piyasa/stop dolumlarının kayma farkı; gözlem yoksa `None` (ölçülmedi, 0 değil).
    pub price: Option<PriceDiffStats>,
    /// Limit gözlemi sayısı.
    pub n_limit: usize,
    /// Simülasyonun doldurduğu limitlerin canlıda dolma oranı; limit gözlemi yoksa `None`.
    pub limit_fill_rate: Option<f64>,
}

/// Kaymanın aleyhte yönlü tick değeri: alışta pahalı, satışta ucuz dolum pozitif.
fn adverse(side: Side, reference: Price, px: Price) -> i64 {
    (px - reference).saturating_mul(i64::from(side.sign()))
}

fn is_price_obs(o: &FillObservation) -> bool {
    o.filled && o.kind != ObservedKind::Limit
}

#[allow(clippy::cast_precision_loss)]
fn mean(v: &[i64]) -> f64 {
    v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64
}

#[allow(clippy::cast_precision_loss)]
fn median(v: &[i64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2] as f64
    } else {
        (s[n / 2 - 1] as f64 + s[n / 2] as f64) / 2.0
    }
}

/// Gözlemlerden kayma ve dolum oranı istatistikleri.
#[allow(clippy::cast_precision_loss)]
pub fn measure(obs: &[FillObservation]) -> SlippageStats {
    let px: Vec<&FillObservation> = obs.iter().filter(|o| is_price_obs(o)).collect();
    let price = (!px.is_empty()).then(|| {
        let live: Vec<i64> = px
            .iter()
            .map(|o| adverse(o.side, o.reference_price, o.live_price))
            .collect();
        let sim: Vec<i64> = px
            .iter()
            .map(|o| adverse(o.side, o.reference_price, o.sim_price))
            .collect();
        let diff: Vec<i64> = sim.iter().zip(&live).map(|(s, l)| s - l).collect();
        let abs: Vec<i64> = px
            .iter()
            .map(|o| (o.sim_price - o.live_price).saturating_abs())
            .collect();
        PriceDiffStats {
            n: px.len(),
            live_mean_adverse_ticks: mean(&live),
            live_median_adverse_ticks: median(&live),
            sim_mean_adverse_ticks: mean(&sim),
            sim_median_adverse_ticks: median(&sim),
            mean_sim_minus_live_ticks: mean(&diff),
            mean_abs_sim_live_diff_ticks: mean(&abs),
        }
    });
    let limits: Vec<&FillObservation> = obs
        .iter()
        .filter(|o| o.kind == ObservedKind::Limit)
        .collect();
    let limit_fill_rate = (!limits.is_empty())
        .then(|| limits.iter().filter(|o| o.filled).count() as f64 / limits.len() as f64);
    SlippageStats {
        price,
        n_limit: limits.len(),
        limit_fill_rate,
    }
}

/// Enstrüman bazında `measure`.
pub fn measure_by_instrument(obs: &[FillObservation]) -> BTreeMap<String, SlippageStats> {
    let mut groups: BTreeMap<&str, Vec<FillObservation>> = BTreeMap::new();
    for o in obs {
        groups.entry(&o.instrument).or_default().push(o.clone());
    }
    groups
        .into_iter()
        .map(|(k, v)| (k.to_string(), measure(&v)))
        .collect()
}

fn default_model() -> String {
    QUEUE_MODEL_NAME.to_string()
}

fn default_min_samples() -> usize {
    10
}

/// Kalibre dolum parametreleri; bir sürüm = bir dosya.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillParams {
    /// Dolum modeli adı (`queue`).
    #[serde(default = "default_model")]
    pub model: String,
    /// Sürüm; her kalibrasyon bir artırır. 1 = kalibre edilmemiş başlangıç.
    pub version: u32,
    /// Bu sürümün türetildiği sürüm.
    #[serde(default)]
    pub parent_version: Option<u32>,
    /// Enstrüman sembolü → piyasa/stop kayması (tick, tek yön, ≥ 0). Yoksa enstrüman varsayılanı.
    #[serde(default)]
    pub slippage_ticks: BTreeMap<String, i64>,
    /// Simülasyonun doldurduğu limitin canlıda dolma olasılığı, (0, 1].
    pub limit_fill_prob: f64,
    /// Lot başına piyasa etkisi, binde tick (adet − 1 lot üzerinden, yukarı yuvarlanır).
    #[serde(default)]
    pub impact_milli_ticks_per_lot: i64,
    /// Bir parametrenin güncellenmesi için gereken asgari gözlem.
    #[serde(default = "default_min_samples")]
    pub min_samples: usize,
    /// Bu sürümü üreten kalibrasyondaki gözlem sayısı.
    #[serde(default)]
    pub n_observations: usize,
}

impl Default for FillParams {
    fn default() -> Self {
        FillParams {
            model: default_model(),
            version: 1,
            parent_version: None,
            slippage_ticks: BTreeMap::new(),
            limit_fill_prob: 1.0,
            impact_milli_ticks_per_lot: 0,
            min_samples: default_min_samples(),
            n_observations: 0,
        }
    }
}

impl FillParams {
    /// Deney kaydına yazılan kimlik, ör. `queue@v3`.
    pub fn id(&self) -> String {
        format!("{}@v{}", self.model, self.version)
    }

    pub fn slippage_for(&self, instrument: &str) -> Option<i64> {
        self.slippage_ticks.get(instrument).copied()
    }

    /// Limit dolum olasılığı binde (1..=1000), aşağı yuvarlanır (karamsar).
    #[allow(clippy::cast_possible_truncation)]
    pub fn limit_fill_prob_milli(&self) -> i64 {
        if self.limit_fill_prob.is_finite() {
            ((self.limit_fill_prob * 1000.0).floor() as i64).clamp(1, 1000)
        } else {
            1
        }
    }

    pub fn impact_ticks(&self, qty: i32) -> i64 {
        impact_ticks(self.impact_milli_ticks_per_lot, qty)
    }

    /// Piyasa/stop emrinin bu parametrelerle simüle dolum fiyatı:
    /// referans + (kayma + etki) aleyhte yönde. Enstrüman kalibre değilse `default_slippage`.
    pub fn sim_aggressive_price(
        &self,
        instrument: &str,
        side: Side,
        reference: Price,
        qty: i32,
        default_slippage: i64,
    ) -> Price {
        let s = self
            .slippage_for(instrument)
            .unwrap_or(default_slippage)
            .saturating_add(self.impact_ticks(qty));
        reference + s.saturating_mul(i64::from(side.sign()))
    }

    pub fn validate(&self) -> Result<(), CalibrationError> {
        let bad = |m: String| Err(CalibrationError::Invalid(m));
        if self.model.is_empty()
            || !self
                .model
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return bad(format!("model adı geçersiz: {:?}", self.model));
        }
        if self.version == 0 {
            return bad("sürüm 0 olamaz".into());
        }
        if !(self.limit_fill_prob > 0.0 && self.limit_fill_prob <= 1.0) {
            return bad(format!(
                "limit_fill_prob (0,1] dışında: {}",
                self.limit_fill_prob
            ));
        }
        if self.impact_milli_ticks_per_lot < 0 {
            return bad("impact_milli_ticks_per_lot negatif".into());
        }
        if let Some((k, v)) = self.slippage_ticks.iter().find(|(_, v)| **v < 0) {
            return bad(format!("{k}: negatif kayma {v}"));
        }
        Ok(())
    }
}

/// Ortanca, yukarı yuvarlanmış tamsayı (karamsar).
fn median_ceil(v: &mut [i64]) -> i64 {
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        let s = v[n / 2 - 1].saturating_add(v[n / 2]);
        // ceil(s / 2), negatif s için de doğru.
        s.div_euclid(2) + s.rem_euclid(2)
    }
}

/// Gözlemlerden yeni parametre sürümü.
///
/// - Kayma: enstrüman başına dolmuş piyasa/stop gözlemlerinde `canlı aleyhte kayma − etki(adet)`
///   ortancası, yukarı yuvarlanır ve 0'ın altına inmez. Gözlem `min_samples`'tan azsa eski değer kalır.
/// - Limit dolum olasılığı: limit gözlemlerinde canlı dolum oranı, 0,01'e **aşağı** yuvarlanır
///   (en az 0,01). Gözlem azsa eski değer kalır.
/// - Piyasa etkisi kalibre edilmez (adet çeşitliliği yok); eski değer taşınır.
/// - Sürüm her çağrıda bir artar (veri yetersiz olsa da kalibrasyon denemesi kayda geçer).
pub fn calibrate(current: &FillParams, obs: &[FillObservation]) -> FillParams {
    let mut next = current.clone();
    next.version = current.version.saturating_add(1);
    next.parent_version = Some(current.version);
    next.n_observations = obs.len();
    let min = current.min_samples.max(1);

    let mut per_inst: BTreeMap<&str, Vec<i64>> = BTreeMap::new();
    for o in obs.iter().filter(|o| is_price_obs(o)) {
        let net = adverse(o.side, o.reference_price, o.live_price)
            .saturating_sub(current.impact_ticks(o.qty));
        per_inst.entry(&o.instrument).or_default().push(net);
    }
    for (inst, mut v) in per_inst {
        if v.len() >= min {
            next.slippage_ticks
                .insert(inst.to_string(), median_ceil(&mut v).max(0));
        }
    }

    let limits: Vec<&FillObservation> = obs
        .iter()
        .filter(|o| o.kind == ObservedKind::Limit)
        .collect();
    if limits.len() >= min {
        let filled = limits.iter().filter(|o| o.filled).count();
        // floor(oran × 100) / 100, tamsayıyla.
        let pct = (filled.saturating_mul(100) / limits.len()).max(1);
        #[allow(clippy::cast_precision_loss)]
        let p = pct as f64 / 100.0;
        next.limit_fill_prob = p;
    }
    next
}

/// Gözlemlerin simülasyon fiyatlarını verilen parametrelerle yeniden hesaplar (piyasa/stop).
/// Kalibrasyon öncesi/sonrası farkı aynı canlı dolumlar üzerinde ölçmek için.
pub fn resimulate(
    params: &FillParams,
    obs: &[FillObservation],
    default_slippage: i64,
) -> Vec<FillObservation> {
    obs.iter()
        .map(|o| {
            let mut o = o.clone();
            if o.kind != ObservedKind::Limit {
                o.sim_price = params.sim_aggressive_price(
                    &o.instrument,
                    o.side,
                    o.reference_price,
                    o.qty,
                    default_slippage,
                );
            }
            o
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum CalibrationError {
    #[error("kalibrasyon dosyası G/Ç hatası ({path}): {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("kalibrasyon JSON hatası ({path}): {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("sürüm zaten var, üzerine yazılmaz: {0}")]
    VersionExists(PathBuf),
    #[error("geçersiz dolum parametresi: {0}")]
    Invalid(String),
}

/// Sürümlü parametre dosyaları: `<dizin>/<model>_v<sürüm 6 hane>.json`. Yalnız ekleme.
#[derive(Debug, Clone)]
pub struct CalibrationStore {
    dir: PathBuf,
}

impl CalibrationStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        CalibrationStore { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path_for(&self, model: &str, version: u32) -> PathBuf {
        self.dir.join(format!("{model}_v{version:06}.json"))
    }

    /// Yeni sürümü yazar. Aynı sürüm varsa hata (eski sürümler değişmez). Yazım atomiktir:
    /// geçici dosya + sabit bağlantı (hedef varsa bağlantı başarısız olur).
    pub fn save(&self, p: &FillParams) -> Result<PathBuf, CalibrationError> {
        p.validate()?;
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| CalibrationError::Io { path, source }
        };
        std::fs::create_dir_all(&self.dir).map_err(io(&self.dir))?;
        let target = self.path_for(&p.model, p.version);
        if target.exists() {
            return Err(CalibrationError::VersionExists(target));
        }
        let json = serde_json::to_vec_pretty(p).map_err(|source| CalibrationError::Json {
            path: target.clone(),
            source,
        })?;
        let tmp = self.dir.join(format!(
            ".{}_v{:06}.{}.tmp",
            p.model,
            p.version,
            std::process::id()
        ));
        std::fs::write(&tmp, &json).map_err(io(&tmp))?;
        let linked = std::fs::hard_link(&tmp, &target);
        let _ = std::fs::remove_file(&tmp);
        match linked {
            Ok(()) => Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(CalibrationError::VersionExists(target))
            }
            Err(source) => Err(CalibrationError::Io {
                path: target,
                source,
            }),
        }
    }

    pub fn load(&self, model: &str, version: u32) -> Result<FillParams, CalibrationError> {
        let path = self.path_for(model, version);
        let bytes = std::fs::read(&path).map_err(|source| CalibrationError::Io {
            path: path.clone(),
            source,
        })?;
        let p: FillParams =
            serde_json::from_slice(&bytes).map_err(|source| CalibrationError::Json {
                path: path.clone(),
                source,
            })?;
        p.validate()?;
        if p.model != model || p.version != version {
            return Err(CalibrationError::Invalid(format!(
                "{} içeriği {} diyor",
                path.display(),
                p.id()
            )));
        }
        Ok(p)
    }

    /// Dizindeki sürümler, artan sırada. Dizin yoksa boş.
    pub fn versions(&self, model: &str) -> Result<Vec<u32>, CalibrationError> {
        let rd = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(CalibrationError::Io {
                    path: self.dir.clone(),
                    source,
                });
            }
        };
        let prefix = format!("{model}_v");
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.map_err(|source| CalibrationError::Io {
                path: self.dir.clone(),
                source,
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(v) = name
                .strip_prefix(&prefix)
                .and_then(|r| r.strip_suffix(".json"))
                .and_then(|r| r.parse::<u32>().ok())
            {
                out.push(v);
            }
        }
        out.sort_unstable();
        Ok(out)
    }

    pub fn latest(&self, model: &str) -> Result<Option<FillParams>, CalibrationError> {
        match self.versions(model)?.last() {
            Some(&v) => self.load(model, v).map(Some),
            None => Ok(None),
        }
    }

    /// Kalibre et ve yeni sürümü yaz. Mevcut sürüm dosyada yoksa önce o da yazılır
    /// (başlangıç sürümü kayıtsız kalmasın).
    pub fn calibrate_and_save(
        &self,
        current: &FillParams,
        obs: &[FillObservation],
    ) -> Result<FillParams, CalibrationError> {
        if !self.path_for(&current.model, current.version).exists() {
            self.save(current)?;
        }
        let next = calibrate(current, obs);
        self.save(&next)?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    const TRUE_SLIP: i64 = 3;

    /// Sentetik "canlı" dolumlar: gerçek kayma 3 tick ± gürültü; simülasyon başlangıç parametresiyle.
    fn synthetic(params: &FillParams, seed: u64) -> Vec<FillObservation> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut v = Vec::new();
        for i in 0..400 {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            let inst = if i % 4 < 2 { "NQ" } else { "GC" };
            let reference = Price(rng.random_range(10_000..20_000));
            let noise: i64 = rng.random_range(-1..=1);
            let live = reference + (TRUE_SLIP + noise) * i64::from(side.sign());
            let kind = if i % 3 == 0 {
                ObservedKind::Stop
            } else {
                ObservedKind::Market
            };
            v.push(FillObservation {
                instrument: inst.to_string(),
                side,
                kind,
                qty: 1,
                reference_price: reference,
                sim_price: params.sim_aggressive_price(inst, side, reference, 1, 1),
                live_price: live,
                filled: true,
            });
        }
        // Limit: simülasyonun doldurduğu 100 emrin 70'i canlıda doldu.
        for i in 0..100 {
            let p = Price(15_000 + i);
            v.push(FillObservation {
                instrument: "NQ".into(),
                side: Side::Buy,
                kind: ObservedKind::Limit,
                qty: 1,
                reference_price: p,
                sim_price: p,
                live_price: p,
                filled: i % 10 < 7,
            });
        }
        v
    }

    /// (e) M4 kabulü: kalibrasyon sonrası |sim − canlı| belirgin küçülür.
    #[test]
    fn m4_calibration_shrinks_sim_live_gap() {
        let start = FillParams {
            slippage_ticks: [("NQ".to_string(), 1), ("GC".to_string(), 1)].into(),
            ..FillParams::default()
        };
        let obs = synthetic(&start, 42);
        let before = measure(&obs);
        let bp = before.price.expect("piyasa gözlemi var");
        let after_params = calibrate(&start, &obs);
        let after = measure(&resimulate(&after_params, &obs, 1));
        let ap = after.price.expect("piyasa gözlemi var");
        eprintln!(
            "önce: |sim−canlı| = {:.3} tick, sim−canlı = {:.3}; sonra: {:.3}, {:.3}; {}",
            bp.mean_abs_sim_live_diff_ticks,
            bp.mean_sim_minus_live_ticks,
            ap.mean_abs_sim_live_diff_ticks,
            ap.mean_sim_minus_live_ticks,
            after_params.id()
        );
        assert!(bp.mean_abs_sim_live_diff_ticks > 1.5);
        assert!(ap.mean_abs_sim_live_diff_ticks < 0.5 * bp.mean_abs_sim_live_diff_ticks);
        assert!(ap.mean_sim_minus_live_ticks.abs() < 0.5);
        assert_eq!(after_params.slippage_for("NQ"), Some(TRUE_SLIP));
        assert_eq!(after_params.slippage_for("GC"), Some(TRUE_SLIP));
        assert_eq!(after_params.version, 2);
        assert_eq!(after_params.id(), "queue@v2");
        // Limit dolum olasılığı.
        assert_eq!(before.n_limit, 100);
        assert!((before.limit_fill_rate.unwrap_or(0.0) - 0.7).abs() < 1e-12);
        assert!((after_params.limit_fill_prob - 0.7).abs() < 1e-12);
        assert_eq!(after_params.limit_fill_prob_milli(), 700);
    }

    #[test]
    fn few_samples_keep_old_values_but_bump_version() {
        let start = FillParams::default();
        let obs: Vec<FillObservation> = synthetic(&start, 1).into_iter().take(5).collect();
        let next = calibrate(&start, &obs);
        assert!(next.slippage_ticks.is_empty());
        assert!((next.limit_fill_prob - 1.0).abs() < 1e-12);
        assert_eq!(next.version, 2);
        assert_eq!(next.parent_version, Some(1));
    }

    #[test]
    fn rounding_is_pessimistic() {
        assert_eq!(median_ceil(&mut [2, 3]), 3);
        assert_eq!(median_ceil(&mut [-3, -2]), -2);
        assert_eq!(median_ceil(&mut [1, 5, 2]), 2);
        let p = FillParams {
            limit_fill_prob: 0.6999,
            ..FillParams::default()
        };
        assert_eq!(p.limit_fill_prob_milli(), 699);
    }

    #[test]
    fn empty_measure_is_unknown_not_zero() {
        let s = measure(&[]);
        assert!(s.price.is_none());
        assert!(s.limit_fill_rate.is_none());
        assert!(measure_by_instrument(&[]).is_empty());
    }

    /// (f) Sürüm artar, eski sürüm dosyası durur ve değişmez.
    #[test]
    fn versions_are_append_only() {
        let dir = tempfile::tempdir().expect("geçici dizin");
        let store = CalibrationStore::new(dir.path().join("var").join("calibration"));
        assert!(store.latest("queue").expect("boş").is_none());
        let v1 = FillParams::default();
        let obs = synthetic(&v1, 7);
        let v2 = store.calibrate_and_save(&v1, &obs).expect("v2");
        let v3 = store.calibrate_and_save(&v2, &obs).expect("v3");
        assert_eq!(store.versions("queue").expect("liste"), vec![1, 2, 3]);
        assert_eq!(v3.id(), "queue@v3");
        let old = store.load("queue", 1).expect("v1 durur");
        assert_eq!(old, v1);
        assert_eq!(store.load("queue", 2).expect("v2"), v2);
        assert_eq!(store.latest("queue").expect("son"), Some(v3));
        // Aynı sürüm ikinci kez yazılamaz.
        let mut tampered = v2.clone();
        tampered.limit_fill_prob = 0.01;
        assert!(matches!(
            store.save(&tampered),
            Err(CalibrationError::VersionExists(_))
        ));
        assert_eq!(store.load("queue", 2).expect("v2 değişmedi"), v2);
        // JSON gidiş-dönüş ve geçici dosya kalmaması.
        let leftovers = std::fs::read_dir(store.dir())
            .expect("dizin")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn invalid_params_rejected() {
        let p = FillParams {
            limit_fill_prob: 0.0,
            ..FillParams::default()
        };
        assert!(p.validate().is_err());
        let p = FillParams {
            model: "../x".into(),
            ..FillParams::default()
        };
        assert!(p.validate().is_err());
    }
}
