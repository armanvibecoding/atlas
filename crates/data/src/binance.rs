//! Binance USDⓈ-M vadeli (ücretsiz, anahtarsız) 1 dakikalık bar çekici.
//!
//! Tick/lot boyutları `exchangeInfo`'dan okunur ve `instruments.toml` ile karşılaştırılır;
//! uyuşmazsa çekim durur (uydurma/varsayım yok).

use crate::quality::check_bars;
use crate::store::{FileHeader, ManifestEntry, Store};
use atlas_core::calendar::{format_utc, from_utc};
use atlas_core::units::NANOS_PER_MIN;
use atlas_core::{Bar, Event, EventKind, Fixed9, Instrument, SessionSpec, UnixNanos};
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use std::time::Duration;

pub const BASE_URL: &str = "https://fapi.binance.com";
const PAGE: u64 = 1500;

#[derive(Debug, thiserror::Error)]
pub enum BinanceError {
    #[error("http: {0}")]
    Http(String),
    #[error("yanıt çözülemedi: {0}")]
    Parse(String),
    #[error(
        "{symbol}: borsa tanımı instruments.toml ile uyuşmuyor ({field}: borsa {exchange}, config {config})"
    )]
    SpecMismatch {
        symbol: String,
        field: &'static str,
        exchange: String,
        config: String,
    },
    #[error("depo: {0}")]
    Store(#[from] crate::store::StoreError),
    #[error("geçersiz ay {0:?} (YYYY-AA)")]
    BadMonth(String),
}

pub struct BinanceFutures {
    client: reqwest::blocking::Client,
    base: String,
    pause: Duration,
}

impl BinanceFutures {
    pub fn new() -> Result<Self, BinanceError> {
        Self::with_base(BASE_URL)
    }

    pub fn with_base(base: &str) -> Result<Self, BinanceError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("atlas-research/0.1")
            .build()
            .map_err(|e| BinanceError::Http(e.to_string()))?;
        Ok(BinanceFutures {
            client,
            base: base.trim_end_matches('/').to_string(),
            pause: Duration::from_millis(150),
        })
    }

    fn get_json(&self, url: &str) -> Result<serde_json::Value, BinanceError> {
        let mut wait = Duration::from_secs(2);
        let mut last = String::new();
        for _ in 0..6 {
            match self.client.get(url).send() {
                Ok(r) if r.status().is_success() => {
                    return r.json().map_err(|e| BinanceError::Parse(e.to_string()));
                }
                Ok(r) => {
                    let s = r.status();
                    last = format!("{s}: {}", r.text().unwrap_or_default());
                    if !(s.as_u16() == 429 || s.as_u16() == 418 || s.is_server_error()) {
                        break;
                    }
                }
                Err(e) => last = e.to_string(),
            }
            std::thread::sleep(wait);
            wait = wait.saturating_mul(2);
        }
        Err(BinanceError::Http(format!("{url}: {last}")))
    }

    /// Borsa sunucu saati (saat kayması kontrolü için).
    pub fn server_time(&self) -> Result<UnixNanos, BinanceError> {
        let v = self.get_json(&format!("{}/fapi/v1/time", self.base))?;
        v.get("serverTime")
            .and_then(serde_json::Value::as_u64)
            .map(UnixNanos::from_millis)
            .ok_or_else(|| BinanceError::Parse("serverTime yok".into()))
    }

    /// (tick boyutu, lot adımı) borsadan.
    pub fn filters(&self, symbol: &str) -> Result<(Fixed9, Fixed9), BinanceError> {
        let v = self.get_json(&format!("{}/fapi/v1/exchangeInfo", self.base))?;
        let syms = v
            .get("symbols")
            .and_then(|s| s.as_array())
            .ok_or_else(|| BinanceError::Parse("symbols yok".into()))?;
        let s = syms
            .iter()
            .find(|s| s.get("symbol").and_then(|x| x.as_str()) == Some(symbol))
            .ok_or_else(|| BinanceError::Parse(format!("{symbol} borsada yok")))?;
        let filters = s
            .get("filters")
            .and_then(|f| f.as_array())
            .ok_or_else(|| BinanceError::Parse("filters yok".into()))?;
        let get = |ty: &str, key: &str| -> Result<Fixed9, BinanceError> {
            let t = filters
                .iter()
                .find(|f| f.get("filterType").and_then(|x| x.as_str()) == Some(ty))
                .and_then(|f| f.get(key))
                .and_then(|x| x.as_str())
                .ok_or_else(|| BinanceError::Parse(format!("{ty}.{key} yok")))?;
            Fixed9::parse(t).map_err(|e| BinanceError::Parse(e.to_string()))
        };
        Ok((
            get("PRICE_FILTER", "tickSize")?,
            get("LOT_SIZE", "stepSize")?,
        ))
    }

    pub fn verify_spec(&self, inst: &Instrument) -> Result<(), BinanceError> {
        let (tick, step) = self.filters(&inst.symbol)?;
        if tick != inst.tick_size {
            return Err(BinanceError::SpecMismatch {
                symbol: inst.symbol.clone(),
                field: "tick_size",
                exchange: tick.to_string(),
                config: inst.tick_size.to_string(),
            });
        }
        if step != inst.lot_size {
            return Err(BinanceError::SpecMismatch {
                symbol: inst.symbol.clone(),
                field: "lot_size",
                exchange: step.to_string(),
                config: inst.lot_size.to_string(),
            });
        }
        Ok(())
    }

    /// `[start, end)` aralığındaki 1m barlar (ms). Sayfalı.
    pub fn klines_1m(
        &self,
        inst: &Instrument,
        start: UnixNanos,
        end: UnixNanos,
    ) -> Result<Vec<Bar>, BinanceError> {
        let mut out: Vec<Bar> = Vec::new();
        let mut cursor_ms = start.0 / 1_000_000;
        let end_ms = end.0 / 1_000_000;
        while cursor_ms < end_ms {
            let url = format!(
                "{}/fapi/v1/klines?symbol={}&interval=1m&startTime={}&endTime={}&limit={}",
                self.base,
                inst.symbol,
                cursor_ms,
                end_ms.saturating_sub(1),
                PAGE
            );
            let v = self.get_json(&url)?;
            let rows = v
                .as_array()
                .ok_or_else(|| BinanceError::Parse("klines dizi değil".into()))?;
            if rows.is_empty() {
                break;
            }
            let mut last_open = cursor_ms;
            for r in rows {
                let b = parse_kline(r, inst)?;
                last_open = b.open_ts.0 / 1_000_000;
                if b.open_ts.0 / 1_000_000 < end_ms {
                    out.push(b);
                }
            }
            cursor_ms = last_open + 60_000;
            std::thread::sleep(self.pause);
        }
        Ok(out)
    }
}

fn field(r: &serde_json::Value, i: usize) -> Result<&serde_json::Value, BinanceError> {
    r.get(i)
        .ok_or_else(|| BinanceError::Parse(format!("kline alanı {i} yok")))
}

fn str_field(r: &serde_json::Value, i: usize) -> Result<&str, BinanceError> {
    field(r, i)?
        .as_str()
        .ok_or_else(|| BinanceError::Parse(format!("kline alanı {i} metin değil")))
}

pub fn parse_kline(r: &serde_json::Value, inst: &Instrument) -> Result<Bar, BinanceError> {
    let open_ms = field(r, 0)?
        .as_u64()
        .ok_or_else(|| BinanceError::Parse("openTime".into()))?;
    let px = |i: usize| -> Result<atlas_core::Price, BinanceError> {
        inst.parse_price(str_field(r, i)?)
            .map_err(|e| BinanceError::Parse(e.to_string()))
    };
    let vol = Fixed9::parse(str_field(r, 5)?).map_err(|e| BinanceError::Parse(e.to_string()))?;
    if inst.lot_size.0 <= 0 || vol.0 % inst.lot_size.0 != 0 {
        return Err(BinanceError::Parse(format!(
            "{}: hacim {vol} lot adımı {}'in katı değil",
            inst.symbol, inst.lot_size
        )));
    }
    let buy = Fixed9::parse(str_field(r, 9)?).map_err(|e| BinanceError::Parse(e.to_string()))?;
    if buy.0 % inst.lot_size.0 != 0 || buy.0 > vol.0 {
        return Err(BinanceError::Parse(format!(
            "{}: taker alış hacmi {buy} geçersiz",
            inst.symbol
        )));
    }
    let trades = field(r, 8)?
        .as_u64()
        .ok_or_else(|| BinanceError::Parse("trades".into()))?;
    let open_ts = UnixNanos::from_millis(open_ms);
    Ok(Bar {
        open_ts,
        close_ts: open_ts.plus(NANOS_PER_MIN),
        open: px(1)?,
        high: px(2)?,
        low: px(3)?,
        close: px(4)?,
        volume: u64::try_from(vol.0 / inst.lot_size.0).unwrap_or(0),
        buy_volume: u64::try_from(buy.0 / inst.lot_size.0).unwrap_or(0),
        trades: u32::try_from(trades).unwrap_or(u32::MAX),
    })
}

/// Ölçülmüş efektif spread ve önerilen kayma (§4.4 kalibrasyon, OPEN_QUESTIONS #6).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SpreadReport {
    pub symbol: String,
    pub windows: usize,
    pub samples: usize,
    /// Agresör yönü değişen ardışık işlemler arası fiyat farkı (tick).
    pub median_spread_ticks: f64,
    pub mean_spread_ticks: f64,
    pub p90_spread_ticks: f64,
    /// Piyasa emrinin son işlem fiyatına göre beklenen maliyeti ≈ yarım spread.
    pub mean_half_spread_ticks: f64,
    /// Tamsayı tick'e yukarı yuvarlanmış öneri (karamsar).
    pub recommended_slippage_ticks: i64,
    pub from: String,
    pub to: String,
}

impl BinanceFutures {
    /// `aggTrades` örneklemesiyle efektif spread ölçer. `windows` adet 60 sn'lik pencere, son
    /// `days` güne eşit aralıklı yayılır (deterministik zamanlar).
    pub fn measure_spread(
        &self,
        inst: &Instrument,
        now: UnixNanos,
        days: u64,
        windows: usize,
    ) -> Result<SpreadReport, BinanceError> {
        let span_ms = days.saturating_mul(86_400_000);
        let end_ms = (now.0 / 1_000_000).saturating_sub(120_000);
        let start_ms = end_ms.saturating_sub(span_ms);
        let step = span_ms / windows.max(1) as u64;
        let mut spreads: Vec<f64> = Vec::new();
        for k in 0..windows {
            let t0 = start_ms + step * k as u64;
            let url = format!(
                "{}/fapi/v1/aggTrades?symbol={}&startTime={}&endTime={}&limit=1000",
                self.base,
                inst.symbol,
                t0,
                t0 + 60_000
            );
            let v = self.get_json(&url)?;
            let rows = v
                .as_array()
                .ok_or_else(|| BinanceError::Parse("aggTrades dizi değil".into()))?;
            let mut prev: Option<(i64, bool, u64)> = None;
            for r in rows {
                let p = r
                    .get("p")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| BinanceError::Parse("aggTrades p".into()))?;
                let px = inst
                    .parse_price(p)
                    .map_err(|e| BinanceError::Parse(e.to_string()))?
                    .0;
                let maker = r
                    .get("m")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let t = r.get("T").and_then(serde_json::Value::as_u64).unwrap_or(0);
                if let Some((pp, pm, pt)) = prev
                    && pm != maker
                    && t.saturating_sub(pt) <= 1_000
                {
                    // m=true: alıcı maker → satıcı agresör (bid'de işlem).
                    let (bid, ask) = if maker { (px, pp) } else { (pp, px) };
                    let s = ask - bid;
                    if s >= 0 {
                        spreads.push(s as f64);
                    }
                }
                prev = Some((px, maker, t));
            }
            std::thread::sleep(self.pause);
        }
        if spreads.is_empty() {
            return Err(BinanceError::Parse("spread örneği yok".into()));
        }
        spreads.sort_by(f64::total_cmp);
        let q = |f: f64| spreads[((spreads.len() - 1) as f64 * f).round() as usize];
        let mean = spreads.iter().sum::<f64>() / spreads.len() as f64;
        let half = mean / 2.0;
        Ok(SpreadReport {
            symbol: inst.symbol.clone(),
            windows,
            samples: spreads.len(),
            median_spread_ticks: q(0.5),
            mean_spread_ticks: mean,
            p90_spread_ticks: q(0.9),
            mean_half_spread_ticks: half,
            recommended_slippage_ticks: (half.ceil() as i64).max(1),
            from: format_utc(UnixNanos::from_millis(start_ms)),
            to: format_utc(UnixNanos::from_millis(end_ms)),
        })
    }
}

pub fn parse_month(s: &str) -> Result<NaiveDate, BinanceError> {
    NaiveDate::parse_from_str(&format!("{}-01", s.trim()), "%Y-%m-%d")
        .map_err(|_| BinanceError::BadMonth(s.to_string()))
}

fn next_month(d: NaiveDate) -> NaiveDate {
    let (y, m) = if d.month() == 12 {
        (d.year() + 1, 1)
    } else {
        (d.year(), d.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1).unwrap_or(d)
}

fn month_start(d: NaiveDate) -> UnixNanos {
    from_utc(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default()))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PullReport {
    pub symbol: String,
    pub months: Vec<ManifestEntry>,
    pub quality_events: Vec<Event>,
}

/// `[from_month, to_month]` aylarını çekip depoya yazar. İçinde bulunulan ay `now`'a kadar.
pub fn pull_months(
    api: &BinanceFutures,
    store: &Store,
    inst: &Instrument,
    session: &SessionSpec,
    from_month: &str,
    to_month: &str,
    now: UnixNanos,
) -> Result<PullReport, BinanceError> {
    api.verify_spec(inst)?;
    let mut m = parse_month(from_month)?;
    let last = parse_month(to_month)?;
    let mut report = PullReport {
        symbol: inst.symbol.clone(),
        months: Vec::new(),
        quality_events: Vec::new(),
    };
    while m <= last {
        let start = month_start(m);
        let mut end = month_start(next_month(m));
        // Yalnız kapanmış dakikalar.
        let now_floor = UnixNanos(now.0 - now.0 % NANOS_PER_MIN);
        if end > now_floor {
            end = now_floor;
        }
        if start >= end {
            break;
        }
        let bars = api.klines_1m(inst, start, end)?;
        if !bars.is_empty() {
            let issues = check_bars(&bars, session, NANOS_PER_MIN);
            for i in &issues {
                report.quality_events.push(i.to_event(&inst.symbol, now));
            }
            let header = FileHeader {
                schema: "ohlcv_1m".into(),
                source: "binance_futures".into(),
                symbol: inst.symbol.clone(),
                tick_size: inst.tick_size,
                lot_size: inst.lot_size,
                interval_ns: NANOS_PER_MIN,
                month: format!("{:04}-{:02}", m.year(), m.month()),
                count: bars.len() as u64,
                first_ts: bars.first().map_or(start, |b| b.open_ts),
                last_ts: bars.last().map_or(start, |b| b.open_ts),
                pulled_at: format_utc(now),
            };
            let entry = store.put_month(&header, &bars, issues.len() as u64)?;
            report.quality_events.push(Event::new(
                now,
                EventKind::DataPulled,
                serde_json::json!({"source": entry.source, "symbol": entry.symbol, "month": entry.month,
                    "bars": entry.count, "version": entry.version, "quality_issues": entry.quality_issues}),
            ));
            report.months.push(entry);
        }
        m = next_month(m);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::InstrumentTable;

    const BTC: &str = r#"
[[instrument]]
symbol = "BTCUSDT"
venue = "binance"
kind = "perpetual"
tick_size = "0.1"
tick_value_usd = "0.0001"
lot_size = "0.001"
session = "always"
"#;

    #[test]
    fn parses_kline_row_exactly() {
        let t = InstrumentTable::from_toml_str(BTC).unwrap();
        let inst = t.get("BTCUSDT").unwrap();
        let row: serde_json::Value = serde_json::from_str(r#"[1793457780000,"77493.80","77493.80","77493.70","77493.80","12.767",1793457839999,"989362.73510",344,"6.672","517038.63360","0"]"#).unwrap();
        let b = parse_kline(&row, inst).unwrap();
        assert_eq!(b.open, atlas_core::Price(774_938));
        assert_eq!(b.low, atlas_core::Price(774_937));
        assert_eq!(b.volume, 12_767);
        assert_eq!(b.buy_volume, 6_672);
        assert_eq!(b.delta(), 2 * 6_672 - 12_767);
        assert_eq!(b.trades, 344);
        assert_eq!(b.close_ts.since(b.open_ts), NANOS_PER_MIN);
    }

    #[test]
    fn months() {
        assert_eq!(
            parse_month("2025-12").unwrap(),
            NaiveDate::from_ymd_opt(2025, 12, 1).unwrap()
        );
        assert_eq!(
            next_month(parse_month("2025-12").unwrap()),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
        assert!(parse_month("2025-13").is_err());
    }
}
