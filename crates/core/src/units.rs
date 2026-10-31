//! Sayısal birimler. Fiyat ve para asla `f64` değildir (kural 1).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

pub const NANOS_PER_SEC: u64 = 1_000_000_000;
pub const NANOS_PER_MIN: u64 = 60 * NANOS_PER_SEC;
pub const NANOS_PER_HOUR: u64 = 60 * NANOS_PER_MIN;
pub const NANOS_PER_DAY: u64 = 24 * NANOS_PER_HOUR;

/// UTC, Unix epoch'tan bu yana nanosaniye.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct UnixNanos(pub u64);

impl UnixNanos {
    pub const ZERO: UnixNanos = UnixNanos(0);
    pub const MAX: UnixNanos = UnixNanos(u64::MAX);

    pub const fn from_secs(s: u64) -> Self {
        UnixNanos(s.saturating_mul(NANOS_PER_SEC))
    }
    pub const fn from_millis(ms: u64) -> Self {
        UnixNanos(ms.saturating_mul(1_000_000))
    }
    pub const fn as_u64(self) -> u64 {
        self.0
    }
    pub const fn plus(self, nanos: u64) -> Self {
        UnixNanos(self.0.saturating_add(nanos))
    }
    pub const fn minus(self, nanos: u64) -> Self {
        UnixNanos(self.0.saturating_sub(nanos))
    }
    /// `self - earlier`, `earlier` daha büyükse 0.
    pub const fn since(self, earlier: UnixNanos) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Display for UnixNanos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Enstrümanın tick'i cinsinden fiyat.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Price(pub i64);

impl Price {
    pub const fn ticks(self) -> i64 {
        self.0
    }
}

impl Add<i64> for Price {
    type Output = Price;
    fn add(self, rhs: i64) -> Price {
        Price(self.0.saturating_add(rhs))
    }
}
impl Sub<i64> for Price {
    type Output = Price;
    fn sub(self, rhs: i64) -> Price {
        Price(self.0.saturating_sub(rhs))
    }
}
impl Sub<Price> for Price {
    /// İki fiyat arasındaki tick farkı.
    type Output = i64;
    fn sub(self, rhs: Price) -> i64 {
        self.0.saturating_sub(rhs.0)
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}t", self.0)
    }
}

/// Kontrat (lot) adedi. İşaretli: pozitif = long, negatif = short.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Qty(pub i32);

impl Qty {
    pub const ZERO: Qty = Qty(0);
    pub const fn abs(self) -> i32 {
        self.0.saturating_abs()
    }
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }
}
impl Add for Qty {
    type Output = Qty;
    fn add(self, rhs: Qty) -> Qty {
        Qty(self.0.saturating_add(rhs.0))
    }
}
impl Sub for Qty {
    type Output = Qty;
    fn sub(self, rhs: Qty) -> Qty {
        Qty(self.0.saturating_sub(rhs.0))
    }
}
impl Neg for Qty {
    type Output = Qty;
    fn neg(self) -> Qty {
        Qty(self.0.saturating_neg())
    }
}

/// Para, mikro-dolar (1 USD = 1_000_000).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Money(pub i64);

pub const MICROS_PER_USD: i64 = 1_000_000;

impl Money {
    pub const ZERO: Money = Money(0);
    pub const fn from_usd(usd: i64) -> Self {
        Money(usd.saturating_mul(MICROS_PER_USD))
    }
    pub const fn micros(self) -> i64 {
        self.0
    }
    /// Yalnız gösterim/istatistik için.
    pub fn as_usd_f64(self) -> f64 {
        self.0 as f64 / MICROS_PER_USD as f64
    }
    /// "12.50" gibi ondalık USD metnini çözer (en fazla 6 ondalık).
    pub fn parse_usd(s: &str) -> Result<Money, UnitError> {
        parse_scaled(s, 6).map(Money)
    }
    pub const fn abs(self) -> Money {
        Money(self.0.saturating_abs())
    }
    pub const fn min(self, other: Money) -> Money {
        if self.0 <= other.0 { self } else { other }
    }
    pub const fn max(self, other: Money) -> Money {
        if self.0 >= other.0 { self } else { other }
    }
}
impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0.saturating_add(rhs.0))
    }
}
impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0.saturating_sub(rhs.0))
    }
}
impl AddAssign for Money {
    fn add_assign(&mut self, rhs: Money) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}
impl SubAssign for Money {
    fn sub_assign(&mut self, rhs: Money) {
        self.0 = self.0.saturating_sub(rhs.0);
    }
}
impl Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money(self.0.saturating_neg())
    }
}
impl Mul<i64> for Money {
    type Output = Money;
    fn mul(self, rhs: i64) -> Money {
        Money(self.0.saturating_mul(rhs))
    }
}
impl std::iter::Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        iter.fold(Money::ZERO, |a, b| a + b)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let a = self.0.unsigned_abs();
        let whole = a / MICROS_PER_USD as u64;
        let frac = a % MICROS_PER_USD as u64;
        write!(f, "{sign}${whole}.{:02}", frac / 10_000)
    }
}

/// 1e-9 ölçekli sabit noktalı ondalık (tick boyutu, lot boyutu gibi tanımlar için).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Fixed9(pub i64);

pub const FIXED9_SCALE: i64 = 1_000_000_000;

impl Fixed9 {
    pub fn parse(s: &str) -> Result<Fixed9, UnitError> {
        parse_scaled(s, 9).map(Fixed9)
    }
    pub fn as_f64(self) -> f64 {
        self.0 as f64 / FIXED9_SCALE as f64
    }
}

impl fmt::Display for Fixed9 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let a = self.0.unsigned_abs();
        let whole = a / FIXED9_SCALE as u64;
        let frac = a % FIXED9_SCALE as u64;
        if frac == 0 {
            return write!(f, "{sign}{whole}");
        }
        let s = format!("{frac:09}");
        write!(f, "{sign}{whole}.{}", s.trim_end_matches('0'))
    }
}

/// Baz puan × 100 (1 bps = 100). Oransal ücretler için tamsayı temsil.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct CentiBps(pub i64);

impl CentiBps {
    /// "0.05%" değil, yüzde metni: "0.05" → %0,05 = 5 bps = 500 centi-bps.
    pub fn parse_percent(s: &str) -> Result<CentiBps, UnitError> {
        // %0,0001 hassasiyet: yüzde * 10_000 = centi-bps
        parse_scaled(s, 4).map(CentiBps)
    }
    /// notional * oran; aşağı değil yukarı yuvarlanır (maliyet asla az sayılmaz).
    pub fn apply(self, notional: Money) -> Money {
        let n = i128::from(notional.0.saturating_abs());
        let r = i128::from(self.0);
        let denom: i128 = 1_000_000; // centi-bps → oran: /10_000 (bps) /100 (centi)
        let v = (n * r + denom - 1) / denom;
        Money(i64::try_from(v).unwrap_or(i64::MAX))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UnitError {
    #[error("boş sayı")]
    Empty,
    #[error("geçersiz sayı: {0:?}")]
    Invalid(String),
    #[error("çok fazla ondalık basamak ({digits} > {max}): {text:?}")]
    TooPrecise {
        text: String,
        digits: usize,
        max: u32,
    },
    #[error("taşma: {0:?}")]
    Overflow(String),
}

/// Ondalık metni `10^scale` ile ölçeklenmiş tamsayıya çevirir. Kayan nokta kullanmaz.
pub fn parse_scaled(s: &str, scale: u32) -> Result<i64, UnitError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(UnitError::Empty);
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(UnitError::Invalid(s.to_string()));
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return Err(UnitError::Invalid(s.to_string()));
    }
    let frac_trimmed = frac_part.trim_end_matches('0');
    if frac_trimmed.len() > scale as usize {
        return Err(UnitError::TooPrecise {
            text: s.to_string(),
            digits: frac_trimmed.len(),
            max: scale,
        });
    }
    let overflow = || UnitError::Overflow(s.to_string());
    let mut v: i64 = 0;
    for c in int_part.chars() {
        let d = i64::from(c as u8 - b'0');
        v = v
            .checked_mul(10)
            .and_then(|x| x.checked_add(d))
            .ok_or_else(overflow)?;
    }
    let mut frac_v: i64 = 0;
    for i in 0..scale as usize {
        let d = frac_trimmed
            .as_bytes()
            .get(i)
            .map_or(0, |b| i64::from(b - b'0'));
        frac_v = frac_v * 10 + d;
    }
    let p = 10_i64.checked_pow(scale).ok_or_else(overflow)?;
    let total = v
        .checked_mul(p)
        .and_then(|x| x.checked_add(frac_v))
        .ok_or_else(overflow)?;
    Ok(if neg { -total } else { total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scaled_basics() {
        assert_eq!(parse_scaled("0.25", 9), Ok(250_000_000));
        assert_eq!(parse_scaled("12.5", 6), Ok(12_500_000));
        assert_eq!(parse_scaled("-3", 2), Ok(-300));
        assert_eq!(parse_scaled("0.00001", 9), Ok(10_000));
        assert_eq!(parse_scaled("1.2300", 2), Ok(123));
        assert!(matches!(
            parse_scaled("1.234", 2),
            Err(UnitError::TooPrecise { .. })
        ));
        assert_eq!(parse_scaled("", 2), Err(UnitError::Empty));
        assert!(parse_scaled("1e5", 2).is_err());
        assert!(parse_scaled(".", 2).is_err());
        assert!(parse_scaled("99999999999999999999", 2).is_err());
    }

    #[test]
    fn money_display() {
        assert_eq!(Money::parse_usd("12.5").unwrap().to_string(), "$12.50");
        assert_eq!(Money(-1_234_567).to_string(), "-$1.23");
    }

    #[test]
    fn fixed9_display() {
        assert_eq!(Fixed9::parse("0.25").unwrap().to_string(), "0.25");
        assert_eq!(Fixed9::parse("1").unwrap().to_string(), "1");
    }

    #[test]
    fn centibps_apply_rounds_up() {
        // %0,05 of $100_000 = $50
        let r = CentiBps::parse_percent("0.05").unwrap();
        assert_eq!(r.apply(Money::from_usd(100_000)), Money::from_usd(50));
        // 1 mikro-dolar notional → en az 1 mikro maliyet
        assert_eq!(r.apply(Money(1)), Money(1));
    }

    #[test]
    fn price_tick_diff() {
        assert_eq!(Price(110) - Price(100), 10);
        assert_eq!(Price(100) + 5, Price(105));
    }
}
