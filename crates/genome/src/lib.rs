//! Strateji genomu (§4.3): TOML ile ifade edilen birleşik yapı — `entry` (sinyal ağacı),
//! `filters`, `exit`, `sizing`. Primitive'ler kapalı kümeden gelir (feature türleri + karşılaştırma
//! operatörleri), her parametrenin tiplenmiş aralığı vardır. Genom → derlenmiş `Strategy`.
//!
//! Örnek:
//! ```toml
//! name = "trend_pullback"
//! instrument = "BTCUSDT"
//! [entry]
//! long  = { all = [ { gt = ["ema:50", "ema:200"] }, { cross_below = ["low", "ema:50"] } ] }
//! short = { all = [ { lt = ["ema:50", "ema:200"] }, { cross_above = ["high", "ema:50"] } ] }
//! [exit]
//! atr = 14
//! stop_atr = 1.5
//! target_r = 2.0
//! time_bars = 240
//! [sizing]
//! risk_usd = 1000
//! ```

pub mod evolve;
pub mod strategy;

use atlas_features::FeatureSpec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Operand {
    Int(i64),
    Num(f64),
    /// `"ema:20"` ya da gecikmeli `"ema:20@1"`.
    Ref(String),
    Op(Arith),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arith {
    Add(Box<(Operand, Operand)>),
    Sub(Box<(Operand, Operand)>),
    Mul(Box<(Operand, Operand)>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cond {
    All(Vec<Cond>),
    Any(Vec<Cond>),
    Not(Box<Cond>),
    Gt(Box<(Operand, Operand)>),
    Lt(Box<(Operand, Operand)>),
    CrossAbove(Box<(Operand, Operand)>),
    CrossBelow(Box<(Operand, Operand)>),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<Cond>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<Cond>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    /// UTC saat penceresi [başlangıç, bitiş); bitiş < başlangıç ise gece yarısını sarar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hours_utc: Option<(u32, u32)>,
    /// ATR (tick) en az bu kadar olmalı.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_atr_ticks: Option<f64>,
    /// Gün (UTC) başına en fazla giriş.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_trades_per_day: Option<u32>,
    /// Hafta kapanışına bu kadar dakika kala giriş yok (ders §9.6; 7/24 piyasada etkisiz).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_entry_before_week_close_min: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    /// Stop mesafesi için ATR periyodu.
    pub atr: u32,
    pub stop_atr: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_r: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_bars: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sizing {
    /// İşlem başı risk (USD, tamsayı). Kontrat sayısı = risk / (stop × tick değeri), aşağı yuvarlanır.
    pub risk_usd: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Genome {
    pub name: String,
    pub instrument: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub entry: Entry,
    #[serde(default)]
    pub filters: Filters,
    pub exit: Exit,
    pub sizing: Sizing,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GenomeError {
    #[error("TOML: {0}")]
    Toml(String),
    #[error("{0}")]
    Invalid(String),
}

/// Çözülmüş feature referansı.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeatureRef {
    pub spec: FeatureSpec,
    pub lag: u32,
}

pub const MAX_LAG: u32 = 50;

pub fn parse_ref(s: &str) -> Result<FeatureRef, GenomeError> {
    let (f, lag) = match s.split_once('@') {
        Some((f, l)) => (
            f,
            l.parse::<u32>()
                .map_err(|_| GenomeError::Invalid(format!("{s}: gecikme tamsayı olmalı")))?,
        ),
        None => (s, 0),
    };
    if lag > MAX_LAG {
        return Err(GenomeError::Invalid(format!("{s}: gecikme ≤ {MAX_LAG}")));
    }
    let spec: FeatureSpec = f.parse().map_err(GenomeError::Invalid)?;
    Ok(FeatureRef { spec, lag })
}

pub fn format_ref(r: &FeatureRef) -> String {
    if r.lag == 0 {
        r.spec.to_string()
    } else {
        format!("{}@{}", r.spec, r.lag)
    }
}

impl Operand {
    fn refs(&self, out: &mut Vec<FeatureRef>) -> Result<(), GenomeError> {
        match self {
            Operand::Int(_) | Operand::Num(_) => Ok(()),
            Operand::Ref(s) => {
                out.push(parse_ref(s)?);
                Ok(())
            }
            Operand::Op(Arith::Add(p) | Arith::Sub(p) | Arith::Mul(p)) => {
                p.0.refs(out)?;
                p.1.refs(out)
            }
        }
    }
}

impl Cond {
    pub fn refs(&self, out: &mut Vec<FeatureRef>) -> Result<(), GenomeError> {
        match self {
            Cond::All(v) | Cond::Any(v) => {
                if v.is_empty() {
                    return Err(GenomeError::Invalid("boş all/any".into()));
                }
                for c in v {
                    c.refs(out)?;
                }
                Ok(())
            }
            Cond::Not(c) => c.refs(out),
            Cond::Gt(p) | Cond::Lt(p) | Cond::CrossAbove(p) | Cond::CrossBelow(p) => {
                p.0.refs(out)?;
                p.1.refs(out)
            }
        }
    }

    /// Yapısal belirteçler (bilgi tabanı benzerliği için).
    pub fn tokens(&self, out: &mut BTreeSet<String>) {
        let op_tokens = |name: &str, p: &(Operand, Operand), out: &mut BTreeSet<String>| {
            let mut r = Vec::new();
            let _ = p.0.refs(&mut r);
            let mut r2 = Vec::new();
            let _ = p.1.refs(&mut r2);
            let k = |v: &Vec<FeatureRef>| {
                v.first()
                    .map_or("const".to_string(), |x| x.spec.kind.name().to_string())
            };
            out.insert(format!("{name}({},{})", k(&r), k(&r2)));
            for x in r.iter().chain(r2.iter()) {
                out.insert(format!("f:{}", x.spec.kind.name()));
            }
        };
        match self {
            Cond::All(v) => {
                out.insert("all".into());
                v.iter().for_each(|c| c.tokens(out));
            }
            Cond::Any(v) => {
                out.insert("any".into());
                v.iter().for_each(|c| c.tokens(out));
            }
            Cond::Not(c) => {
                out.insert("not".into());
                c.tokens(out);
            }
            Cond::Gt(p) => op_tokens("gt", p, out),
            Cond::Lt(p) => op_tokens("lt", p, out),
            Cond::CrossAbove(p) => op_tokens("xa", p, out),
            Cond::CrossBelow(p) => op_tokens("xb", p, out),
        }
    }
}

impl Genome {
    pub fn from_toml_str(s: &str) -> Result<Genome, GenomeError> {
        let g: Genome = toml::from_str(s).map_err(|e| GenomeError::Toml(e.to_string()))?;
        g.validate()?;
        Ok(g)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), GenomeError> {
        if self.entry.long.is_none() && self.entry.short.is_none() {
            return Err(GenomeError::Invalid(
                "entry.long ya da entry.short gerekli".into(),
            ));
        }
        self.feature_refs()?;
        let e = &self.exit;
        if !(0.05..=50.0).contains(&e.stop_atr) {
            return Err(GenomeError::Invalid("exit.stop_atr 0.05..50".into()));
        }
        if let Some(t) = e.target_r
            && !(0.1..=50.0).contains(&t)
        {
            return Err(GenomeError::Invalid("exit.target_r 0.1..50".into()));
        }
        if !(2..=500).contains(&e.atr) {
            return Err(GenomeError::Invalid("exit.atr 2..500".into()));
        }
        if self.sizing.risk_usd == 0 {
            return Err(GenomeError::Invalid("sizing.risk_usd > 0".into()));
        }
        if let Some((a, b)) = self.filters.hours_utc
            && (a > 24 || b > 24)
        {
            return Err(GenomeError::Invalid("filters.hours_utc 0..24".into()));
        }
        Ok(())
    }

    /// Genomun kullandığı tüm feature referansları (+ stop için ATR).
    pub fn feature_refs(&self) -> Result<Vec<FeatureRef>, GenomeError> {
        let mut v = Vec::new();
        if let Some(c) = &self.entry.long {
            c.refs(&mut v)?;
        }
        if let Some(c) = &self.entry.short {
            c.refs(&mut v)?;
        }
        v.push(FeatureRef {
            spec: FeatureSpec::new(atlas_features::Kind::Atr, self.exit.atr),
            lag: 0,
        });
        Ok(v)
    }

    pub fn feature_specs(&self) -> Result<Vec<FeatureSpec>, GenomeError> {
        let mut s: Vec<FeatureSpec> = self.feature_refs()?.into_iter().map(|r| r.spec).collect();
        s.sort();
        s.dedup();
        Ok(s)
    }

    /// İçerik hash'i (isim ve etiketler hariç: aynı yapı = aynı hash).
    pub fn hash(&self) -> String {
        let mut c = self.clone();
        c.name.clear();
        c.tags.clear();
        let bytes = serde_json::to_vec(&c).unwrap_or_default();
        Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn tokens(&self) -> BTreeSet<String> {
        let mut t = BTreeSet::new();
        if let Some(c) = &self.entry.long {
            t.insert("long".into());
            c.tokens(&mut t);
        }
        if let Some(c) = &self.entry.short {
            t.insert("short".into());
            c.tokens(&mut t);
        }
        if self.exit.target_r.is_some() {
            t.insert("exit:target".into());
        }
        if self.exit.time_bars.is_some() {
            t.insert("exit:time".into());
        }
        if self.filters.hours_utc.is_some() {
            t.insert("filter:hours".into());
        }
        t
    }
}

/// İki genomun yapısal mesafesi: 1 − Jaccard(belirteçler).
pub fn structural_distance(a: &Genome, b: &Genome) -> f64 {
    let (ta, tb) = (a.tokens(), b.tokens());
    let inter = ta.intersection(&tb).count() as f64;
    let uni = ta.union(&tb).count() as f64;
    if uni == 0.0 { 0.0 } else { 1.0 - inter / uni }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const TREND: &str = r#"
name = "trend_pullback"
instrument = "BTCUSDT"
tags = ["trend"]
[entry]
long = { all = [ { gt = ["ema:50", "ema:200"] }, { cross_below = ["low", "ema:50"] } ] }
short = { all = [ { lt = ["ema:50", "ema:200"] }, { cross_above = ["high", { add = ["ema:50", 0] }] } ] }
[filters]
hours_utc = [7, 20]
[exit]
atr = 14
stop_atr = 1.5
target_r = 2.0
time_bars = 240
[sizing]
risk_usd = 1000
"#;

    #[test]
    fn parses_validates_and_roundtrips() {
        let g = Genome::from_toml_str(TREND).unwrap();
        let specs: Vec<String> = g
            .feature_specs()
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(specs, vec!["high", "low", "ema:50", "ema:200", "atr:14"]);
        let back = Genome::from_toml_str(&g.to_toml()).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.hash(), g.hash());
        let mut renamed = g.clone();
        renamed.name = "x".into();
        assert_eq!(renamed.hash(), g.hash());
    }

    #[test]
    fn rejects_bad_genomes() {
        assert!(Genome::from_toml_str(&TREND.replace("ema:200", "ema:1")).is_err());
        assert!(
            Genome::from_toml_str(&TREND.replace("ema:50\", \"ema:200", "nope\", \"ema:200"))
                .is_err()
        );
        assert!(Genome::from_toml_str(&TREND.replace("stop_atr = 1.5", "stop_atr = 0")).is_err());
        assert!(
            Genome::from_toml_str(&TREND.replace("low\", \"ema:50", "low@99\", \"ema:50")).is_err()
        );
    }

    #[test]
    fn distance() {
        let g = Genome::from_toml_str(TREND).unwrap();
        assert_eq!(structural_distance(&g, &g), 0.0);
        let mut h = g.clone();
        h.entry.short = None;
        h.exit.target_r = None;
        let d = structural_distance(&g, &h);
        assert!(d > 0.0 && d < 1.0, "{d}");
    }
}
