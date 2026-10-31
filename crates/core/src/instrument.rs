//! Enstrüman tanımı ve `configs/instruments.toml` yükleyici.
//!
//! Maliyet alanları (komisyon, borsa ücreti, kayma) **uydurulmaz**: boş bırakılırsa `None` olur ve
//! maliyet gerektiren her koşu açık hata verir (spec §8).

use crate::units::{CentiBps, Fixed9, Money, Price, Qty, UnitError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Bellek içi enstrüman kimliği. Sembollerin alfabetik sırasıyla atanır (deterministik).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct InstrumentId(pub u32);

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    Cme,
    Binance,
    Bybit,
    Dukascopy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    Future,
    Perpetual,
    Cfd,
    Spot,
}

/// İşlem maliyeti tanımı. `None` = doldurulmadı.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostSpec {
    /// Kontrat başına, tek yön (giriş veya çıkış) komisyon.
    pub commission_per_contract: Option<Money>,
    /// Kontrat başına, tek yön borsa/regülasyon ücreti.
    pub exchange_fee_per_contract: Option<Money>,
    /// Oransal ücret (kripto/CFD), tek yön, notional üzerinden.
    pub fee_rate: Option<CentiBps>,
    /// Varsayılan kayma (tick, tek yön, piyasa/stop emirlerinde).
    pub slippage_ticks: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instrument {
    pub id: InstrumentId,
    pub symbol: String,
    pub venue: Venue,
    pub kind: InstrumentKind,
    /// Fiyat adımı (ondalık).
    pub tick_size: Fixed9,
    /// `Qty(1)` için bir tick'lik hareketin değeri.
    pub tick_value: Money,
    /// `Qty(1)`'in temsil ettiği dayanak miktarı (vadeli = 1, BTCUSDT = 0.001).
    pub lot_size: Fixed9,
    /// `core::calendar` içindeki seans tanımının adı.
    pub session: String,
    /// Net maruziyet grubu (ders §9.5). Aynı gruptaki enstrümanlar birlikte sayılır.
    pub exposure_group: Option<String>,
    /// Gruptaki ağırlık, binde (NQ = 1000, MNQ = 100).
    pub exposure_weight_milli: i64,
    pub costs: CostSpec,
    /// Ücretsiz veri kaynağındaki karşılık (ör. Dukascopy sembolü). Bilgi amaçlı.
    pub data_symbol: Option<String>,
}

impl Instrument {
    /// Ondalık fiyatı tick'e çevirir; tick'in tam katı değilse hata.
    pub fn price_from_fixed9(&self, p: Fixed9) -> Result<Price, InstrumentError> {
        if self.tick_size.0 <= 0 {
            return Err(InstrumentError::BadTick(self.symbol.clone()));
        }
        if p.0 % self.tick_size.0 != 0 {
            return Err(InstrumentError::OffTick {
                symbol: self.symbol.clone(),
                price: p,
                tick: self.tick_size,
            });
        }
        Ok(Price(p.0 / self.tick_size.0))
    }

    pub fn price_to_fixed9(&self, p: Price) -> Fixed9 {
        Fixed9(p.0.saturating_mul(self.tick_size.0))
    }

    /// Ondalık metin fiyatı tick'e çevirir.
    pub fn parse_price(&self, s: &str) -> Result<Price, InstrumentError> {
        let f = Fixed9::parse(s).map_err(|e| InstrumentError::Unit(self.symbol.clone(), e))?;
        self.price_from_fixed9(f)
    }

    /// `qty` pozisyonun `from`→`to` hareketindeki brüt PnL'i. `qty` işaretli.
    pub fn pnl(&self, from: Price, to: Price, qty: Qty) -> Money {
        let ticks = i128::from(to - from) * i128::from(qty.0);
        let v = ticks * i128::from(self.tick_value.0);
        Money(clamp_i128(v))
    }

    /// Mutlak notional değer (fiyat × lot × adet).
    pub fn notional(&self, price: Price, qty: Qty) -> Money {
        let v = i128::from(price.0) * i128::from(self.tick_value.0) * i128::from(qty.abs());
        Money(clamp_i128(v.abs()))
    }

    /// Tek yön işlem ücreti (komisyon + borsa ücreti + oransal ücret). Kayma hariç.
    /// Herhangi bir gerekli alan doldurulmadıysa hata.
    pub fn fee(&self, price: Price, qty: Qty) -> Result<Money, InstrumentError> {
        let c = &self.costs;
        let n = i64::from(qty.abs());
        let has_any = c.commission_per_contract.is_some()
            || c.exchange_fee_per_contract.is_some()
            || c.fee_rate.is_some();
        if !has_any {
            return Err(InstrumentError::CostMissing {
                symbol: self.symbol.clone(),
                field: "commission_per_contract_usd / exchange_fee_per_contract_usd / fee_rate_percent",
            });
        }
        let mut total = Money::ZERO;
        if let Some(m) = c.commission_per_contract {
            total += m * n;
        }
        if let Some(m) = c.exchange_fee_per_contract {
            total += m * n;
        }
        if let Some(r) = c.fee_rate {
            total += r.apply(self.notional(price, qty));
        }
        Ok(total)
    }

    pub fn slippage_ticks(&self) -> Result<i64, InstrumentError> {
        self.costs
            .slippage_ticks
            .ok_or_else(|| InstrumentError::CostMissing {
                symbol: self.symbol.clone(),
                field: "slippage_ticks",
            })
    }
}

fn clamp_i128(v: i128) -> i64 {
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InstrumentError {
    #[error("{0}: geçersiz tick boyutu")]
    BadTick(String),
    #[error("{symbol}: fiyat {price} tick {tick}'in katı değil")]
    OffTick {
        symbol: String,
        price: Fixed9,
        tick: Fixed9,
    },
    #[error("{0}: sayı hatası: {1}")]
    Unit(String, UnitError),
    #[error("{symbol}: maliyet alanı doldurulmamış ({field}) — instruments.toml, uydurulmaz")]
    CostMissing { symbol: String, field: &'static str },
    #[error("instruments.toml çözülemedi: {0}")]
    Toml(String),
    #[error("{symbol}: zorunlu alan eksik/boş: {field}")]
    Missing { symbol: String, field: &'static str },
    #[error("yinelenen sembol: {0}")]
    Duplicate(String),
    #[error("bilinmeyen sembol: {0}")]
    Unknown(String),
}

#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(default)]
    instrument: Vec<RawInstrument>,
}

#[derive(Debug, Deserialize)]
struct RawInstrument {
    symbol: String,
    venue: Venue,
    kind: InstrumentKind,
    tick_size: String,
    tick_value_usd: String,
    #[serde(default = "one")]
    lot_size: String,
    session: String,
    #[serde(default)]
    exposure_group: Option<String>,
    #[serde(default = "one")]
    exposure_weight: String,
    #[serde(default)]
    data_symbol: Option<String>,
    #[serde(default)]
    costs: RawCosts,
}

fn one() -> String {
    "1".to_string()
}

#[derive(Debug, Default, Deserialize)]
struct RawCosts {
    #[serde(default)]
    commission_per_contract_usd: String,
    #[serde(default)]
    exchange_fee_per_contract_usd: String,
    #[serde(default)]
    fee_rate_percent: String,
    #[serde(default)]
    slippage_ticks: String,
}

/// Yüklü enstrüman tablosu. Sembol → kimlik eşlemesi alfabetik ve deterministiktir.
#[derive(Debug, Clone, Default)]
pub struct InstrumentTable {
    by_symbol: BTreeMap<String, Instrument>,
    by_id: Vec<String>,
}

impl InstrumentTable {
    pub fn from_toml_str(text: &str) -> Result<Self, InstrumentError> {
        let raw: RawFile =
            toml::from_str(text).map_err(|e| InstrumentError::Toml(e.to_string()))?;
        let mut parsed: BTreeMap<String, Instrument> = BTreeMap::new();
        for r in raw.instrument {
            let sym = r.symbol.trim().to_string();
            if sym.is_empty() {
                return Err(InstrumentError::Missing {
                    symbol: "?".into(),
                    field: "symbol",
                });
            }
            let unit = |e: UnitError| InstrumentError::Unit(sym.clone(), e);
            let req = |s: &str, field: &'static str| -> Result<String, InstrumentError> {
                let t = s.trim();
                if t.is_empty() {
                    Err(InstrumentError::Missing {
                        symbol: sym.clone(),
                        field,
                    })
                } else {
                    Ok(t.to_string())
                }
            };
            let tick_size = Fixed9::parse(&req(&r.tick_size, "tick_size")?).map_err(unit)?;
            let tick_value =
                Money::parse_usd(&req(&r.tick_value_usd, "tick_value_usd")?).map_err(unit)?;
            let lot_size = Fixed9::parse(&req(&r.lot_size, "lot_size")?).map_err(unit)?;
            let weight =
                crate::units::parse_scaled(&req(&r.exposure_weight, "exposure_weight")?, 3)
                    .map_err(unit)?;
            if tick_size.0 <= 0 || tick_value.0 <= 0 || lot_size.0 <= 0 {
                return Err(InstrumentError::BadTick(sym));
            }
            let opt_money = |s: &str| -> Result<Option<Money>, InstrumentError> {
                if s.trim().is_empty() {
                    Ok(None)
                } else {
                    Money::parse_usd(s).map(Some).map_err(unit)
                }
            };
            let costs = CostSpec {
                commission_per_contract: opt_money(&r.costs.commission_per_contract_usd)?,
                exchange_fee_per_contract: opt_money(&r.costs.exchange_fee_per_contract_usd)?,
                fee_rate: if r.costs.fee_rate_percent.trim().is_empty() {
                    None
                } else {
                    Some(CentiBps::parse_percent(&r.costs.fee_rate_percent).map_err(unit)?)
                },
                slippage_ticks: if r.costs.slippage_ticks.trim().is_empty() {
                    None
                } else {
                    Some(crate::units::parse_scaled(&r.costs.slippage_ticks, 0).map_err(unit)?)
                },
            };
            let inst = Instrument {
                id: InstrumentId(0),
                symbol: sym.clone(),
                venue: r.venue,
                kind: r.kind,
                tick_size,
                tick_value,
                lot_size,
                session: req(&r.session, "session")?,
                exposure_group: r.exposure_group.filter(|g| !g.trim().is_empty()),
                exposure_weight_milli: weight,
                costs,
                data_symbol: r.data_symbol.filter(|g| !g.trim().is_empty()),
            };
            if parsed.insert(sym.clone(), inst).is_some() {
                return Err(InstrumentError::Duplicate(sym));
            }
        }
        Ok(Self::from_instruments(parsed.into_values()))
    }

    /// Kimlikleri sembol sırasına göre yeniden atar.
    pub fn from_instruments(items: impl IntoIterator<Item = Instrument>) -> Self {
        let mut by_symbol: BTreeMap<String, Instrument> =
            items.into_iter().map(|i| (i.symbol.clone(), i)).collect();
        let mut by_id = Vec::with_capacity(by_symbol.len());
        for (idx, (sym, inst)) in by_symbol.iter_mut().enumerate() {
            inst.id = InstrumentId(u32::try_from(idx).unwrap_or(u32::MAX));
            by_id.push(sym.clone());
        }
        InstrumentTable { by_symbol, by_id }
    }

    pub fn get(&self, symbol: &str) -> Result<&Instrument, InstrumentError> {
        self.by_symbol
            .get(symbol)
            .ok_or_else(|| InstrumentError::Unknown(symbol.to_string()))
    }

    pub fn by_id(&self, id: InstrumentId) -> Option<&Instrument> {
        self.by_id
            .get(id.0 as usize)
            .and_then(|s| self.by_symbol.get(s))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Instrument> {
        self.by_symbol.values()
    }

    pub fn len(&self) -> usize {
        self.by_symbol.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_symbol.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[instrument]]
symbol = "NQ"
venue = "cme"
kind = "future"
tick_size = "0.25"
tick_value_usd = "5.00"
session = "cme_globex"
exposure_group = "us_tech"
exposure_weight = "1"
[instrument.costs]
commission_per_contract_usd = ""
slippage_ticks = ""

[[instrument]]
symbol = "BTCUSDT"
venue = "binance"
kind = "perpetual"
tick_size = "0.1"
tick_value_usd = "0.0001"
lot_size = "0.001"
session = "always"
[instrument.costs]
fee_rate_percent = "0.05"
slippage_ticks = "1"
"#;

    #[test]
    fn loads_and_assigns_sorted_ids() {
        let t = InstrumentTable::from_toml_str(SAMPLE).unwrap();
        assert_eq!(t.get("BTCUSDT").unwrap().id, InstrumentId(0));
        assert_eq!(t.get("NQ").unwrap().id, InstrumentId(1));
        assert_eq!(t.by_id(InstrumentId(1)).unwrap().symbol, "NQ");
    }

    #[test]
    fn price_conversion_exact() {
        let t = InstrumentTable::from_toml_str(SAMPLE).unwrap();
        let nq = t.get("NQ").unwrap();
        assert_eq!(nq.parse_price("21000.25").unwrap(), Price(84001));
        assert!(matches!(
            nq.parse_price("21000.10"),
            Err(InstrumentError::OffTick { .. })
        ));
        assert_eq!(nq.price_to_fixed9(Price(84001)).to_string(), "21000.25");
    }

    #[test]
    fn pnl_and_missing_costs() {
        let t = InstrumentTable::from_toml_str(SAMPLE).unwrap();
        let nq = t.get("NQ").unwrap();
        // 4 tick × 2 kontrat × $5 = $40
        assert_eq!(nq.pnl(Price(100), Price(104), Qty(2)), Money::from_usd(40));
        assert_eq!(
            nq.pnl(Price(100), Price(104), Qty(-2)),
            Money::from_usd(-40)
        );
        assert!(matches!(
            nq.fee(Price(100), Qty(1)),
            Err(InstrumentError::CostMissing { .. })
        ));
        assert!(nq.slippage_ticks().is_err());
    }

    #[test]
    fn crypto_rate_fee() {
        let t = InstrumentTable::from_toml_str(SAMPLE).unwrap();
        let btc = t.get("BTCUSDT").unwrap();
        // 60000.0 USDT = 600000 tick; 1 lot = 0.001 BTC → notional $60
        let p = btc.parse_price("60000").unwrap();
        assert_eq!(btc.notional(p, Qty(1)), Money::from_usd(60));
        // %0,05 → $0.03
        assert_eq!(btc.fee(p, Qty(1)).unwrap(), Money(30_000));
    }

    #[test]
    fn missing_required_field_errors() {
        let bad = SAMPLE.replace("tick_value_usd = \"5.00\"", "tick_value_usd = \"\"");
        assert!(matches!(
            InstrumentTable::from_toml_str(&bad),
            Err(InstrumentError::Missing { .. })
        ));
    }
}
