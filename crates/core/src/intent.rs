//! Strateji niyeti (`Intent`) ve dolum (`Fill`) tipleri.
//!
//! Strateji emir değil `Intent` üretir; `RiskGate` onaylar, `Executor` emre çevirir (§4.3).

use crate::instrument::InstrumentId;
use crate::units::{Money, Price, Qty, UnixNanos};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    /// Buy = +1, Sell = -1.
    pub const fn sign(self) -> i32 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }
    pub const fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
    /// Pozisyon işaretinden yön.
    pub const fn of_position(q: Qty) -> Option<Side> {
        if q.0 > 0 {
            Some(Side::Buy)
        } else if q.0 < 0 {
            Some(Side::Sell)
        } else {
            None
        }
    }
}

/// Strateji kimliği. `Arc<str>`: sıcak yolda kopyalamak ucuz (yalnız sayaç artar).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StrategyId(pub std::sync::Arc<str>);

impl StrategyId {
    pub fn new(s: impl AsRef<str>) -> Self {
        StrategyId(std::sync::Arc::from(s.as_ref()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StrategyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "price")]
pub enum EntryType {
    /// Bir sonraki fiyattan.
    Market,
    /// Limit: alışta bu fiyat veya altı.
    Limit(Price),
    /// Stop: alışta bu fiyat veya üstüne çıkınca piyasa.
    Stop(Price),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum Action {
    /// Yeni pozisyon aç. Stop zorunlu: risk sınırlanamayan giriş yoktur.
    Open {
        side: Side,
        /// Pozitif adet.
        qty: i32,
        entry: EntryType,
        stop_loss: Price,
        take_profit: Option<Price>,
        /// Bu andan sonra pozisyon piyasadan kapatılır.
        time_exit: Option<UnixNanos>,
        /// Bekleyen giriş emri bu anda iptal olur.
        entry_expiry: Option<UnixNanos>,
    },
    /// Stratejinin bu enstrümandaki pozisyonunu kapat.
    Close { reason: String },
    /// Bekleyen giriş emrini iptal et.
    CancelEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub strategy: StrategyId,
    pub instrument: InstrumentId,
    pub ts: UnixNanos,
    pub action: Action,
}

impl Intent {
    /// Riski azaltan niyet mi (kapanış/iptal)? Bunlar risk kapısında reddedilmez.
    pub fn is_risk_reducing(&self) -> bool {
        matches!(self.action, Action::Close { .. } | Action::CancelEntry)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillReason {
    Entry,
    StopLoss,
    TakeProfit,
    TimeExit,
    Close,
    KillSwitch,
    SessionEnd,
    /// Veri bitti: son fiyattan, kaymasız kapanış (yalnız simülasyon sonu).
    EndOfData,
}

impl FillReason {
    pub const fn is_exit(self) -> bool {
        !matches!(self, FillReason::Entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    pub ts: UnixNanos,
    pub strategy: StrategyId,
    pub instrument: InstrumentId,
    pub side: Side,
    /// Pozitif adet.
    pub qty: i32,
    pub price: Price,
    /// Bu dolumun ücreti (komisyon + borsa + oransal). Kayma fiyatın içindedir.
    pub fee: Money,
    pub reason: FillReason,
}

impl Fill {
    /// Pozisyona etkisi (işaretli).
    pub fn signed_qty(&self) -> Qty {
        Qty(self.qty.saturating_mul(self.side.sign()))
    }
}
