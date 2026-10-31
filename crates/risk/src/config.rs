//! Prop kuralları (`configs/props/<firma>_<boyut>.toml`) ve Atlas'ın kendi risk politikası.
//!
//! **Prop değerleri uydurulmaz** (§4.5): şablondaki her alan zorunludur; boş (`""`) veya eksik alan
//! varsa config yüklenmez ve hata hepsini listeler. Bir kural firmada yoksa açıkça `"none"` yazılır.

use atlas_core::calendar::{CalendarError, parse_tz};
use atlas_core::units::{Money, UnitError, parse_scaled};
use chrono_tz::Tz;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("TOML çözülemedi: {0}")]
    Toml(String),
    #[error("prop config doldurulmamış — boş/eksik alanlar: {}", .0.join(", "))]
    Blank(Vec<String>),
    #[error("alan {field}: {msg}")]
    Invalid { field: String, msg: String },
}

fn invalid(field: &str, msg: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        field: field.to_string(),
        msg: msg.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawdownType {
    Static,
    EodTrailing,
    IntradayTrailing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DailyLossReference {
    DayStartBalance,
    DayStartEquity,
}

/// Gün içi yerel saat (dakika).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct LocalMinute(pub u32);

fn parse_hhmm(field: &str, s: &str) -> Result<LocalMinute, ConfigError> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| invalid(field, format!("SS:DD bekleniyor, {s:?}")))?;
    let h: u32 = h
        .trim()
        .parse()
        .map_err(|_| invalid(field, "saat sayı değil"))?;
    let m: u32 = m
        .trim()
        .parse()
        .map_err(|_| invalid(field, "dakika sayı değil"))?;
    if h > 23 || m > 59 {
        return Err(invalid(field, "saat aralık dışı"));
    }
    Ok(LocalMinute(h * 60 + m))
}

/// Firma kuralları. Yüklendikten sonra değiştirilemez (setter yok).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PropRules {
    pub firm: String,
    pub account_label: String,
    pub starting_balance: Money,
    pub profit_target: Money,
    pub drawdown_type: DrawdownType,
    pub drawdown_amount: Money,
    /// Takip eden taban bu bakiyeye ulaşınca sabitlenir. `None` = kilit yok.
    pub drawdown_lock_level: Option<Money>,
    pub daily_loss_limit: Option<Money>,
    pub daily_loss_reference: DailyLossReference,
    /// Gün sınırı (firmanın günü bu saat diliminde bu saatte biter).
    pub day_reset_tz: Tz,
    pub day_reset_time: LocalMinute,
    /// Tek günün kârı / toplam kâr ≤ oran (binde). `None` = kural yok.
    pub consistency_ratio_milli: Option<i64>,
    /// Ağırlıklı toplam kontrat (enstrüman `exposure_weight` ile; mini = 1, mikro = 0.1).
    pub max_contracts_milli: i64,
    /// Kural saatleri bu saat diliminde.
    pub rules_tz: Tz,
    /// Bu saatten sonra yeni giriş yok. `None` = kısıt yok.
    pub no_new_entries_after: Option<LocalMinute>,
    /// Bu saatte tüm pozisyonlar kapanmış olmalı. `None` = kısıt yok.
    pub flat_by: Option<LocalMinute>,
    pub weekend_holding_allowed: bool,
    /// Haber yasağı penceresi (önce, sonra) dakika. `None` = haber kuralı yok.
    pub news_blackout_minutes: Option<(u32, u32)>,
    pub algo_trading_allowed: bool,
}

const REQUIRED: &[&str] = &[
    "firm",
    "account_label",
    "starting_balance_usd",
    "profit_target_usd",
    "drawdown_type",
    "drawdown_amount_usd",
    "drawdown_lock_level_usd",
    "daily_loss_limit_usd",
    "daily_loss_reference",
    "day_reset_tz",
    "day_reset_time",
    "consistency_ratio",
    "max_contracts",
    "rules_tz",
    "no_new_entries_after",
    "flat_by",
    "weekend_holding_allowed",
    "news_blackout_minutes_before",
    "news_blackout_minutes_after",
    "algo_trading_allowed",
];

fn value_text(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.trim().to_string(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    }
}

fn is_none(s: &str) -> bool {
    s.eq_ignore_ascii_case("none")
}

impl PropRules {
    pub fn from_toml_str(text: &str) -> Result<PropRules, ConfigError> {
        let table: BTreeMap<String, toml::Value> =
            toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        let mut blanks = Vec::new();
        let mut vals: BTreeMap<&str, String> = BTreeMap::new();
        for key in REQUIRED {
            match table.get(*key).map(value_text) {
                Some(s) if !s.is_empty() => {
                    vals.insert(key, s);
                }
                _ => blanks.push((*key).to_string()),
            }
        }
        if !blanks.is_empty() {
            return Err(ConfigError::Blank(blanks));
        }
        let g = |k: &str| vals.get(k).cloned().unwrap_or_default();
        let money = |k: &str| -> Result<Money, ConfigError> {
            Money::parse_usd(&g(k)).map_err(|e: UnitError| invalid(k, e.to_string()))
        };
        let opt_money = |k: &str| -> Result<Option<Money>, ConfigError> {
            let s = g(k);
            if is_none(&s) {
                Ok(None)
            } else {
                money(k).map(Some)
            }
        };
        let boolean = |k: &str| -> Result<bool, ConfigError> {
            match g(k).to_ascii_lowercase().as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(invalid(k, "true/false bekleniyor")),
            }
        };
        let tz = |k: &str| -> Result<Tz, ConfigError> {
            parse_tz(&g(k)).map_err(|e: CalendarError| invalid(k, e.to_string()))
        };
        let opt_time = |k: &str| -> Result<Option<LocalMinute>, ConfigError> {
            let s = g(k);
            if is_none(&s) {
                Ok(None)
            } else {
                parse_hhmm(k, &s).map(Some)
            }
        };

        let drawdown_type = match g("drawdown_type").as_str() {
            "static" => DrawdownType::Static,
            "eod_trailing" => DrawdownType::EodTrailing,
            "intraday_trailing" => DrawdownType::IntradayTrailing,
            other => {
                return Err(invalid(
                    "drawdown_type",
                    format!("static|eod_trailing|intraday_trailing bekleniyor, {other:?}"),
                ));
            }
        };
        let daily_loss_reference = match g("daily_loss_reference").as_str() {
            "day_start_balance" => DailyLossReference::DayStartBalance,
            "day_start_equity" => DailyLossReference::DayStartEquity,
            other => {
                return Err(invalid(
                    "daily_loss_reference",
                    format!("day_start_balance|day_start_equity bekleniyor, {other:?}"),
                ));
            }
        };
        let consistency_ratio_milli = {
            let s = g("consistency_ratio");
            if is_none(&s) {
                None
            } else {
                let v =
                    parse_scaled(&s, 3).map_err(|e| invalid("consistency_ratio", e.to_string()))?;
                if v <= 0 || v >= 1000 {
                    return Err(invalid("consistency_ratio", "0 ile 1 arasında olmalı"));
                }
                Some(v)
            }
        };
        let max_contracts_milli = parse_scaled(&g("max_contracts"), 3)
            .map_err(|e| invalid("max_contracts", e.to_string()))?;
        if max_contracts_milli <= 0 {
            return Err(invalid("max_contracts", "pozitif olmalı"));
        }
        let news = {
            let b = g("news_blackout_minutes_before");
            let a = g("news_blackout_minutes_after");
            if is_none(&b) && is_none(&a) {
                None
            } else {
                let pb: u32 = b
                    .parse()
                    .map_err(|_| invalid("news_blackout_minutes_before", "tamsayı veya none"))?;
                let pa: u32 = a
                    .parse()
                    .map_err(|_| invalid("news_blackout_minutes_after", "tamsayı veya none"))?;
                Some((pb, pa))
            }
        };
        let rules = PropRules {
            firm: g("firm"),
            account_label: g("account_label"),
            starting_balance: money("starting_balance_usd")?,
            profit_target: money("profit_target_usd")?,
            drawdown_type,
            drawdown_amount: money("drawdown_amount_usd")?,
            drawdown_lock_level: opt_money("drawdown_lock_level_usd")?,
            daily_loss_limit: opt_money("daily_loss_limit_usd")?,
            daily_loss_reference,
            day_reset_tz: tz("day_reset_tz")?,
            day_reset_time: parse_hhmm("day_reset_time", &g("day_reset_time"))?,
            consistency_ratio_milli,
            max_contracts_milli,
            rules_tz: tz("rules_tz")?,
            no_new_entries_after: opt_time("no_new_entries_after")?,
            flat_by: opt_time("flat_by")?,
            weekend_holding_allowed: boolean("weekend_holding_allowed")?,
            news_blackout_minutes: news,
            algo_trading_allowed: boolean("algo_trading_allowed")?,
        };
        if rules.starting_balance.0 <= 0 || rules.drawdown_amount.0 <= 0 {
            return Err(invalid(
                "starting_balance_usd/drawdown_amount_usd",
                "pozitif olmalı",
            ));
        }
        Ok(rules)
    }
}

/// Atlas'ın kendi (firmadan bağımsız) risk politikası, `atlas.toml [risk]`.
///
/// Alanlar özeldir; çalışma anında yalnız [`RiskPolicy::tighten`] ile **sıkılaştırılabilir** (kural 5).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RiskPolicy {
    cooldown_after_stop_min: u32,
    max_risk_per_trade: Money,
    /// Grup başına ağırlıklı net maruziyet tavanı (binde kontrat).
    max_group_net_milli: i64,
    /// Bugünkü consistency tavanının bu oranına (binde) gelince yeni giriş kısılır.
    consistency_headroom_milli: i64,
    /// Tabana uzaklık drawdown'un bu oranının (binde) altına inince uyarı.
    drawdown_near_milli: i64,
    /// En kötü durum hesabına eklenen kayma tamponu (tick).
    stop_slippage_buffer_ticks: i64,
    /// Hafta kapanışına bu kadar dakika kala yeni giriş yok (hafta sonu tutma yasaksa).
    min_minutes_before_week_close: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TightenOp {
    CooldownAfterStopMin(u32),
    MaxRiskPerTrade(Money),
    MaxGroupNetMilli(i64),
    ConsistencyHeadroomMilli(i64),
    DrawdownNearMilli(i64),
    StopSlippageBufferTicks(i64),
    MinMinutesBeforeWeekClose(u32),
}

impl RiskPolicy {
    pub fn from_toml_str(text: &str) -> Result<RiskPolicy, ConfigError> {
        #[derive(serde::Deserialize)]
        struct Raw {
            risk: BTreeMap<String, toml::Value>,
        }
        let raw: Raw = toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        let keys = [
            "cooldown_after_stop_minutes",
            "max_risk_per_trade_usd",
            "max_group_net_contracts",
            "consistency_headroom",
            "drawdown_near",
            "stop_slippage_buffer_ticks",
            "min_minutes_before_week_close",
        ];
        let mut blanks = Vec::new();
        let mut v: BTreeMap<&str, String> = BTreeMap::new();
        for k in keys {
            match raw.risk.get(k).map(value_text) {
                Some(s) if !s.is_empty() => {
                    v.insert(k, s);
                }
                _ => blanks.push(format!("risk.{k}")),
            }
        }
        if !blanks.is_empty() {
            return Err(ConfigError::Blank(blanks));
        }
        let g = |k: &str| v.get(k).cloned().unwrap_or_default();
        let int = |k: &str| -> Result<u32, ConfigError> {
            g(k).parse().map_err(|_| invalid(k, "pozitif tamsayı"))
        };
        let frac = |k: &str| -> Result<i64, ConfigError> {
            let x = parse_scaled(&g(k), 3).map_err(|e| invalid(k, e.to_string()))?;
            if !(0..=1000).contains(&x) {
                return Err(invalid(k, "0..1 arası"));
            }
            Ok(x)
        };
        Ok(RiskPolicy {
            cooldown_after_stop_min: int("cooldown_after_stop_minutes")?,
            max_risk_per_trade: Money::parse_usd(&g("max_risk_per_trade_usd"))
                .map_err(|e| invalid("max_risk_per_trade_usd", e.to_string()))?,
            max_group_net_milli: parse_scaled(&g("max_group_net_contracts"), 3)
                .map_err(|e| invalid("max_group_net_contracts", e.to_string()))?,
            consistency_headroom_milli: frac("consistency_headroom")?,
            drawdown_near_milli: frac("drawdown_near")?,
            stop_slippage_buffer_ticks: i64::from(int("stop_slippage_buffer_ticks")?),
            min_minutes_before_week_close: int("min_minutes_before_week_close")?,
        })
    }

    pub fn cooldown_after_stop_min(&self) -> u32 {
        self.cooldown_after_stop_min
    }
    pub fn max_risk_per_trade(&self) -> Money {
        self.max_risk_per_trade
    }
    pub fn max_group_net_milli(&self) -> i64 {
        self.max_group_net_milli
    }
    pub fn consistency_headroom_milli(&self) -> i64 {
        self.consistency_headroom_milli
    }
    pub fn drawdown_near_milli(&self) -> i64 {
        self.drawdown_near_milli
    }
    pub fn stop_slippage_buffer_ticks(&self) -> i64 {
        self.stop_slippage_buffer_ticks
    }
    pub fn min_minutes_before_week_close(&self) -> u32 {
        self.min_minutes_before_week_close
    }

    /// Yalnız sıkılaştırır. Gevşetme girişimi hata döner ve hiçbir şey değişmez.
    pub fn tighten(&mut self, op: TightenOp) -> Result<(), ConfigError> {
        fn check<T: PartialOrd + std::fmt::Debug>(
            name: &str,
            ok: bool,
            old: T,
            new: T,
        ) -> Result<(), ConfigError> {
            if ok {
                Ok(())
            } else {
                Err(invalid(
                    name,
                    format!("gevşetme yasak (eski {old:?}, yeni {new:?})"),
                ))
            }
        }
        match op {
            TightenOp::CooldownAfterStopMin(n) => {
                check(
                    "cooldown",
                    n >= self.cooldown_after_stop_min,
                    self.cooldown_after_stop_min,
                    n,
                )?;
                self.cooldown_after_stop_min = n;
            }
            TightenOp::MaxRiskPerTrade(m) => {
                check(
                    "max_risk_per_trade",
                    m <= self.max_risk_per_trade,
                    self.max_risk_per_trade,
                    m,
                )?;
                self.max_risk_per_trade = m;
            }
            TightenOp::MaxGroupNetMilli(m) => {
                check(
                    "max_group_net",
                    m <= self.max_group_net_milli,
                    self.max_group_net_milli,
                    m,
                )?;
                self.max_group_net_milli = m;
            }
            TightenOp::ConsistencyHeadroomMilli(m) => {
                check(
                    "consistency_headroom",
                    m <= self.consistency_headroom_milli,
                    self.consistency_headroom_milli,
                    m,
                )?;
                self.consistency_headroom_milli = m;
            }
            TightenOp::DrawdownNearMilli(m) => {
                check(
                    "drawdown_near",
                    m >= self.drawdown_near_milli,
                    self.drawdown_near_milli,
                    m,
                )?;
                self.drawdown_near_milli = m;
            }
            TightenOp::StopSlippageBufferTicks(t) => {
                check(
                    "stop_slippage_buffer",
                    t >= self.stop_slippage_buffer_ticks,
                    self.stop_slippage_buffer_ticks,
                    t,
                )?;
                self.stop_slippage_buffer_ticks = t;
            }
            TightenOp::MinMinutesBeforeWeekClose(n) => {
                check(
                    "min_minutes_before_week_close",
                    n >= self.min_minutes_before_week_close,
                    self.min_minutes_before_week_close,
                    n,
                )?;
                self.min_minutes_before_week_close = n;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Testlere özel, gerçek bir firmaya ait OLMAYAN kurgusal kurallar.
    pub const TEST_PROP: &str = r#"
firm = "TEST-FIRMA (kurgusal)"
account_label = "test_50k"
starting_balance_usd = "50000"
profit_target_usd = "3000"
drawdown_type = "eod_trailing"
drawdown_amount_usd = "2000"
drawdown_lock_level_usd = "50000"
daily_loss_limit_usd = "1000"
daily_loss_reference = "day_start_balance"
day_reset_tz = "America/Chicago"
day_reset_time = "17:00"
consistency_ratio = "0.5"
max_contracts = "5"
rules_tz = "America/Chicago"
no_new_entries_after = "15:00"
flat_by = "15:10"
weekend_holding_allowed = false
news_blackout_minutes_before = "2"
news_blackout_minutes_after = "2"
algo_trading_allowed = true
"#;

    pub const TEST_POLICY: &str = r#"
[risk]
cooldown_after_stop_minutes = 15
max_risk_per_trade_usd = "400"
max_group_net_contracts = "5"
consistency_headroom = "0.8"
drawdown_near = "0.25"
stop_slippage_buffer_ticks = 2
min_minutes_before_week_close = 60
"#;

    #[test]
    fn loads_complete_config() {
        let r = PropRules::from_toml_str(TEST_PROP).unwrap();
        assert_eq!(r.starting_balance, Money::from_usd(50_000));
        assert_eq!(r.drawdown_type, DrawdownType::EodTrailing);
        assert_eq!(r.consistency_ratio_milli, Some(500));
        assert_eq!(r.max_contracts_milli, 5000);
        assert_eq!(r.no_new_entries_after, Some(LocalMinute(15 * 60)));
        assert_eq!(r.news_blackout_minutes, Some((2, 2)));
    }

    #[test]
    fn blank_fields_refuse_to_load_and_are_all_listed() {
        let t = TEST_PROP
            .replace(
                "drawdown_amount_usd = \"2000\"",
                "drawdown_amount_usd = \"\"",
            )
            .replace("max_contracts = \"5\"\n", "");
        match PropRules::from_toml_str(&t) {
            Err(ConfigError::Blank(v)) => {
                assert!(v.contains(&"drawdown_amount_usd".to_string()));
                assert!(v.contains(&"max_contracts".to_string()));
            }
            other => panic!("beklenmeyen: {other:?}"),
        }
    }

    #[test]
    fn none_is_explicit() {
        let t = TEST_PROP
            .replace(
                "consistency_ratio = \"0.5\"",
                "consistency_ratio = \"none\"",
            )
            .replace(
                "daily_loss_limit_usd = \"1000\"",
                "daily_loss_limit_usd = \"none\"",
            );
        let r = PropRules::from_toml_str(&t).unwrap();
        assert_eq!(r.consistency_ratio_milli, None);
        assert_eq!(r.daily_loss_limit, None);
    }

    #[test]
    fn invalid_values_rejected() {
        let t = TEST_PROP.replace("\"eod_trailing\"", "\"weird\"");
        assert!(matches!(
            PropRules::from_toml_str(&t),
            Err(ConfigError::Invalid { .. })
        ));
        let t = TEST_PROP.replace("consistency_ratio = \"0.5\"", "consistency_ratio = \"1.5\"");
        assert!(PropRules::from_toml_str(&t).is_err());
    }

    #[test]
    fn policy_only_tightens() {
        let mut p = RiskPolicy::from_toml_str(TEST_POLICY).unwrap();
        assert!(
            p.tighten(TightenOp::MaxRiskPerTrade(Money::from_usd(300)))
                .is_ok()
        );
        assert_eq!(p.max_risk_per_trade(), Money::from_usd(300));
        assert!(
            p.tighten(TightenOp::MaxRiskPerTrade(Money::from_usd(500)))
                .is_err()
        );
        assert_eq!(p.max_risk_per_trade(), Money::from_usd(300));
        assert!(p.tighten(TightenOp::CooldownAfterStopMin(5)).is_err());
        assert!(p.tighten(TightenOp::CooldownAfterStopMin(30)).is_ok());
        assert!(p.tighten(TightenOp::DrawdownNearMilli(100)).is_err());
        assert!(p.tighten(TightenOp::MaxGroupNetMilli(9000)).is_err());
        assert!(p.tighten(TightenOp::ConsistencyHeadroomMilli(900)).is_err());
        assert!(p.tighten(TightenOp::StopSlippageBufferTicks(1)).is_err());
        assert!(p.tighten(TightenOp::MinMinutesBeforeWeekClose(10)).is_err());
    }
}
