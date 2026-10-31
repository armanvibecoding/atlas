//! Feature tanımı: `ad[:parametre]` (ör. `ema:20`, `atr:14`, `vwap`, `pdh`).
//! Kapalı küme; her türün tiplenmiş parametre aralığı vardır (evrim bu aralıkta kalır).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

macro_rules! kinds {
    ($( $v:ident => $name:literal, $lo:expr, $hi:expr, $def:expr; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum Kind { $( $v, )* }

        impl Kind {
            pub const ALL: &'static [Kind] = &[ $( Kind::$v, )* ];
            pub fn name(self) -> &'static str { match self { $( Kind::$v => $name, )* } }
            /// (alt, üst, varsayılan) parametre; alt = üst = 0 → parametresiz.
            pub fn range(self) -> (u32, u32, u32) { match self { $( Kind::$v => ($lo, $hi, $def), )* } }
            pub fn from_name(s: &str) -> Option<Kind> { match s { $( $name => Some(Kind::$v), )* _ => None } }
        }
    };
}

kinds! {
    Open => "open", 0, 0, 0;
    High => "high", 0, 0, 0;
    Low => "low", 0, 0, 0;
    Close => "close", 0, 0, 0;
    Volume => "volume", 0, 0, 0;
    Delta => "delta", 0, 0, 0;
    Imbalance => "imbalance", 0, 0, 0;
    Ema => "ema", 2, 1000, 20;
    Sma => "sma", 2, 1000, 20;
    Atr => "atr", 2, 500, 14;
    Rsi => "rsi", 2, 200, 14;
    Highest => "highest", 2, 2000, 20;
    Lowest => "lowest", 2, 2000, 20;
    Vwap => "vwap", 0, 0, 0;
    Cvd => "cvd", 0, 0, 0;
    Pdh => "pdh", 0, 0, 0;
    Pdl => "pdl", 0, 0, 0;
    AsiaHigh => "asia_high", 0, 0, 0;
    AsiaLow => "asia_low", 0, 0, 0;
    LondonHigh => "london_high", 0, 0, 0;
    LondonLow => "london_low", 0, 0, 0;
    OrHigh => "or_high", 5, 240, 30;
    OrLow => "or_low", 5, 240, 30;
    Poc => "poc", 0, 0, 0;
    Vah => "vah", 0, 0, 0;
    Val => "val", 0, 0, 0;
    Absorption => "absorption", 3, 500, 20;
    SweepLow => "sweep_low", 2, 2000, 20;
    SweepHigh => "sweep_high", 2, 2000, 20;
    CvdDivBull => "cvd_div_bull", 2, 2000, 20;
    CvdDivBear => "cvd_div_bear", 2, 2000, 20;
    MinuteOfDay => "minute_of_day", 0, 0, 0;
}

impl Kind {
    pub fn has_param(self) -> bool {
        self.range().1 > 0
    }
}

/// Bir feature türü + parametresi. Sürüm, hesap mantığı değiştikçe artar (önbellek anahtarı).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FeatureSpec {
    pub kind: Kind,
    pub param: u32,
}

pub const FEATURE_VERSION: u32 = 1;

impl FeatureSpec {
    pub fn new(kind: Kind, param: u32) -> Self {
        let (lo, hi, _) = kind.range();
        FeatureSpec {
            kind,
            param: if kind.has_param() {
                param.clamp(lo, hi)
            } else {
                0
            },
        }
    }
}

impl fmt::Display for FeatureSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.kind.has_param() {
            write!(f, "{}:{}", self.kind.name(), self.param)
        } else {
            f.write_str(self.kind.name())
        }
    }
}

impl FromStr for FeatureSpec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, p) = match s.trim().split_once(':') {
            Some((n, p)) => (n, Some(p)),
            None => (s.trim(), None),
        };
        let kind = Kind::from_name(name).ok_or_else(|| format!("bilinmeyen feature: {name}"))?;
        let (lo, hi, def) = kind.range();
        let param = match (kind.has_param(), p) {
            (false, None) => 0,
            (false, Some(_)) => return Err(format!("{name} parametre almaz")),
            (true, None) => def,
            (true, Some(p)) => {
                let v: u32 = p
                    .parse()
                    .map_err(|_| format!("{s}: parametre tamsayı olmalı"))?;
                if v < lo || v > hi {
                    return Err(format!("{s}: parametre {lo}..{hi} aralığında olmalı"));
                }
                v
            }
        };
        Ok(FeatureSpec { kind, param })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_display() {
        let s: FeatureSpec = "ema:20".parse().unwrap();
        assert_eq!(s.to_string(), "ema:20");
        assert_eq!("vwap".parse::<FeatureSpec>().unwrap().to_string(), "vwap");
        assert_eq!("atr".parse::<FeatureSpec>().unwrap().param, 14);
        assert!("ema:1".parse::<FeatureSpec>().is_err());
        assert!("vwap:3".parse::<FeatureSpec>().is_err());
        assert!("nope".parse::<FeatureSpec>().is_err());
        for k in Kind::ALL {
            assert_eq!(Kind::from_name(k.name()), Some(*k));
        }
    }
}
