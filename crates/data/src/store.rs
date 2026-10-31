//! Yerel bar deposu (ADR 0001): ay başına bir `.atb` dosyası + `manifest.json`.
//!
//! Dosya: `ATLASBAR` (8) | sürüm u32 | başlık uzunluğu u32 | başlık JSON | kayıtlar (72 bayt, LE).
//! `DataVersion` = SHA-256(şema | sembol | tick | lot | kayıt baytları). Başlıktaki çekim zamanı vb.
//! meta veriler hash'e girmez: aynı içerik = aynı versiyon.

use atlas_core::{Bar, Fixed9, InstrumentId, MarketEvent, Price, UnixNanos};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 8] = b"ATLASBAR";
pub const FORMAT_VERSION: u32 = 2;
pub const BAR_RECORD_LEN: usize = 72;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io {path}: {err}")]
    Io { path: String, err: std::io::Error },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bozuk dosya {path}: {msg}")]
    Corrupt { path: String, msg: String },
    #[error("veri bulunamadı: {0}")]
    NotFound(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> StoreError + '_ {
    move |err| StoreError::Io {
        path: path.display().to_string(),
        err,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHeader {
    pub schema: String,
    pub source: String,
    pub symbol: String,
    pub tick_size: Fixed9,
    pub lot_size: Fixed9,
    pub interval_ns: u64,
    pub month: String,
    pub count: u64,
    pub first_ts: UnixNanos,
    pub last_ts: UnixNanos,
    /// Bilgi amaçlı; hash'e girmez.
    pub pulled_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DataVersion(pub String);

impl DataVersion {
    /// Birden çok parçanın birleşik versiyonu (sıra önemli).
    pub fn combine(parts: &[DataVersion]) -> DataVersion {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p.0.as_bytes());
            h.update(b"\n");
        }
        DataVersion(hex(&h.finalize()))
    }

    pub fn short(&self) -> &str {
        self.0.get(..12).unwrap_or(&self.0)
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn encode_bar(b: &Bar, out: &mut Vec<u8>) {
    out.extend_from_slice(&b.open_ts.0.to_le_bytes());
    out.extend_from_slice(&b.close_ts.0.to_le_bytes());
    out.extend_from_slice(&b.open.0.to_le_bytes());
    out.extend_from_slice(&b.high.0.to_le_bytes());
    out.extend_from_slice(&b.low.0.to_le_bytes());
    out.extend_from_slice(&b.close.0.to_le_bytes());
    out.extend_from_slice(&b.volume.to_le_bytes());
    out.extend_from_slice(&b.buy_volume.to_le_bytes());
    out.extend_from_slice(&b.trades.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
}

fn le_u64(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    if let Some(s) = b.get(at..at + 8) {
        a.copy_from_slice(s);
    }
    u64::from_le_bytes(a)
}

fn le_i64(b: &[u8], at: usize) -> i64 {
    le_u64(b, at) as i64
}

fn le_u32(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    if let Some(s) = b.get(at..at + 4) {
        a.copy_from_slice(s);
    }
    u32::from_le_bytes(a)
}

pub fn decode_bar(r: &[u8]) -> Bar {
    Bar {
        open_ts: UnixNanos(le_u64(r, 0)),
        close_ts: UnixNanos(le_u64(r, 8)),
        open: Price(le_i64(r, 16)),
        high: Price(le_i64(r, 24)),
        low: Price(le_i64(r, 32)),
        close: Price(le_i64(r, 40)),
        volume: le_u64(r, 48),
        buy_volume: le_u64(r, 56),
        trades: le_u32(r, 64),
    }
}

pub fn content_version(h: &FileHeader, records: &[u8]) -> DataVersion {
    let mut d = Sha256::new();
    d.update(h.schema.as_bytes());
    d.update(b"|");
    d.update(h.symbol.as_bytes());
    d.update(b"|");
    d.update(h.tick_size.0.to_le_bytes());
    d.update(h.lot_size.0.to_le_bytes());
    d.update(h.interval_ns.to_le_bytes());
    d.update(records);
    DataVersion(hex(&d.finalize()))
}

pub fn write_bar_file(
    path: &Path,
    header: &FileHeader,
    bars: &[Bar],
) -> Result<DataVersion, StoreError> {
    let mut rec = Vec::with_capacity(bars.len() * BAR_RECORD_LEN);
    for b in bars {
        encode_bar(b, &mut rec);
    }
    let h = serde_json::to_vec(header)?;
    let mut buf = Vec::with_capacity(16 + h.len() + rec.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    buf.extend_from_slice(&u32::try_from(h.len()).unwrap_or(u32::MAX).to_le_bytes());
    buf.extend_from_slice(&h);
    buf.extend_from_slice(&rec);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io(dir))?;
    }
    // Atomik yazım: geçici dosya + yeniden adlandırma.
    let tmp = path.with_extension("atb.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(io(&tmp))?;
        f.write_all(&buf).map_err(io(&tmp))?;
        f.sync_all().map_err(io(&tmp))?;
    }
    fs::rename(&tmp, path).map_err(io(path))?;
    Ok(content_version(header, &rec))
}

pub fn read_bar_file(path: &Path) -> Result<(FileHeader, Vec<Bar>, DataVersion), StoreError> {
    let mut buf = Vec::new();
    fs::File::open(path)
        .map_err(io(path))?
        .read_to_end(&mut buf)
        .map_err(io(path))?;
    let corrupt = |msg: &str| StoreError::Corrupt {
        path: path.display().to_string(),
        msg: msg.to_string(),
    };
    if buf.len() < 16 || buf.get(..8) != Some(MAGIC.as_slice()) {
        return Err(corrupt("sihirli bayt yok"));
    }
    if le_u32(&buf, 8) != FORMAT_VERSION {
        return Err(corrupt("bilinmeyen format sürümü"));
    }
    let hl = le_u32(&buf, 12) as usize;
    let hbytes = buf.get(16..16 + hl).ok_or_else(|| corrupt("başlık kısa"))?;
    let header: FileHeader = serde_json::from_slice(hbytes)?;
    let rec = buf.get(16 + hl..).ok_or_else(|| corrupt("kayıt yok"))?;
    if rec.len() % BAR_RECORD_LEN != 0 || (rec.len() / BAR_RECORD_LEN) as u64 != header.count {
        return Err(corrupt("kayıt sayısı başlıkla uyuşmuyor"));
    }
    let bars = rec
        .as_chunks::<BAR_RECORD_LEN>()
        .0
        .iter()
        .map(|c| decode_bar(c))
        .collect();
    let v = content_version(&header, rec);
    Ok((header, bars, v))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub file: String,
    pub source: String,
    pub symbol: String,
    pub schema: String,
    pub month: String,
    pub count: u64,
    pub first_ts: UnixNanos,
    pub last_ts: UnixNanos,
    pub version: DataVersion,
    pub quality_issues: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Anahtar: göreli dosya yolu.
    pub entries: BTreeMap<String, ManifestEntry>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn rel_path(source: &str, symbol: &str, schema: &str, month: &str) -> String {
        format!("{source}/{symbol}/{schema}/{month}.atb")
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    pub fn manifest(&self) -> Result<Manifest, StoreError> {
        let p = self.manifest_path();
        if !p.exists() {
            return Ok(Manifest::default());
        }
        let text = fs::read_to_string(&p).map_err(io(&p))?;
        Ok(serde_json::from_str(&text)?)
    }

    fn save_manifest(&self, m: &Manifest) -> Result<(), StoreError> {
        fs::create_dir_all(&self.root).map_err(io(&self.root))?;
        let p = self.manifest_path();
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(m)?).map_err(io(&tmp))?;
        fs::rename(&tmp, &p).map_err(io(&p))?;
        Ok(())
    }

    pub fn put_month(
        &self,
        header: &FileHeader,
        bars: &[Bar],
        quality_issues: u64,
    ) -> Result<ManifestEntry, StoreError> {
        let rel = Self::rel_path(
            &header.source,
            &header.symbol,
            &header.schema,
            &header.month,
        );
        let version = write_bar_file(&self.root.join(&rel), header, bars)?;
        let entry = ManifestEntry {
            file: rel.clone(),
            source: header.source.clone(),
            symbol: header.symbol.clone(),
            schema: header.schema.clone(),
            month: header.month.clone(),
            count: header.count,
            first_ts: header.first_ts,
            last_ts: header.last_ts,
            version,
            quality_issues,
        };
        let mut m = self.manifest()?;
        m.entries.insert(rel, entry.clone());
        self.save_manifest(&m)?;
        Ok(entry)
    }

    /// Bir sembolün `[from, to)` aralığındaki barları ve birleşik versiyonu.
    /// Her dosyanın içerik hash'i manifestle karşılaştırılır (sessiz bozulma yok).
    pub fn load_bars(
        &self,
        source: &str,
        symbol: &str,
        schema: &str,
        from: UnixNanos,
        to: UnixNanos,
    ) -> Result<(Vec<Bar>, DataVersion), StoreError> {
        let m = self.manifest()?;
        let mut entries: Vec<&ManifestEntry> = m
            .entries
            .values()
            .filter(|e| {
                e.source == source
                    && e.symbol == symbol
                    && e.schema == schema
                    && e.last_ts >= from
                    && e.first_ts < to
            })
            .collect();
        entries.sort_by(|a, b| a.month.cmp(&b.month));
        if entries.is_empty() {
            return Err(StoreError::NotFound(format!(
                "{source}/{symbol}/{schema} {from}..{to}"
            )));
        }
        let mut bars = Vec::new();
        let mut versions = Vec::new();
        for e in entries {
            let (_, b, v) = read_bar_file(&self.root.join(&e.file))?;
            if v != e.version {
                return Err(StoreError::Corrupt {
                    path: e.file.clone(),
                    msg: format!(
                        "içerik hash'i manifestle uyuşmuyor ({} ≠ {})",
                        v.short(),
                        e.version.short()
                    ),
                });
            }
            versions.push(v);
            bars.extend(
                b.into_iter()
                    .filter(|x| x.open_ts >= from && x.open_ts < to),
            );
        }
        Ok((bars, DataVersion::combine(&versions)))
    }
}

/// Barları piyasa olaylarına çevirir (olay zamanı = bar kapanışı).
pub fn bars_to_events(inst: InstrumentId, bars: &[Bar]) -> Vec<MarketEvent> {
    bars.iter().map(|b| MarketEvent::bar(inst, *b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars() -> Vec<Bar> {
        (0..10u64)
            .map(|i| Bar {
                open_ts: UnixNanos(i * 60_000_000_000),
                close_ts: UnixNanos((i + 1) * 60_000_000_000),
                open: Price(100 + i as i64),
                high: Price(105 + i as i64),
                low: Price(95 + i as i64),
                close: Price(101 + i as i64),
                volume: 10 + i,
                buy_volume: 0,
                trades: 3,
            })
            .collect()
    }

    fn header(n: u64, pulled: &str) -> FileHeader {
        FileHeader {
            schema: "ohlcv_1m".into(),
            source: "test".into(),
            symbol: "X".into(),
            tick_size: Fixed9(100_000_000),
            lot_size: Fixed9(1_000_000),
            interval_ns: 60_000_000_000,
            month: "1970-01".into(),
            count: n,
            first_ts: UnixNanos(0),
            last_ts: UnixNanos(9 * 60_000_000_000),
            pulled_at: pulled.into(),
        }
    }

    #[test]
    fn roundtrip_and_version_ignores_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.atb");
        let v1 = write_bar_file(&p, &header(10, "t1"), &bars()).unwrap();
        let (h, b, v) = read_bar_file(&p).unwrap();
        assert_eq!(b, bars());
        assert_eq!(h.count, 10);
        assert_eq!(v, v1);
        let v2 = write_bar_file(&dir.path().join("b.atb"), &header(10, "t2"), &bars()).unwrap();
        assert_eq!(v1, v2);
        let mut changed = bars();
        changed[3].close = Price(1);
        let v3 = write_bar_file(&dir.path().join("c.atb"), &header(10, "t1"), &changed).unwrap();
        assert_ne!(v1, v3);
    }

    #[test]
    fn store_detects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::new(dir.path());
        let e = s.put_month(&header(10, "t"), &bars(), 0).unwrap();
        let (b, v) = s
            .load_bars("test", "X", "ohlcv_1m", UnixNanos(0), UnixNanos(u64::MAX))
            .unwrap();
        assert_eq!(b.len(), 10);
        assert_eq!(v, DataVersion::combine(std::slice::from_ref(&e.version)));
        // Dosyayı değiştir
        let path = dir.path().join(&e.file);
        let mut raw = fs::read(&path).unwrap();
        let n = raw.len();
        raw[n - 20] ^= 0xff;
        fs::write(&path, raw).unwrap();
        assert!(matches!(
            s.load_bars("test", "X", "ohlcv_1m", UnixNanos(0), UnixNanos(u64::MAX)),
            Err(StoreError::Corrupt { .. })
        ));
    }
}
