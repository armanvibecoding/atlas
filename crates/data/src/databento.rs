//! Databento maliyet kapısı (§4.1): indirmeden önce maliyet sorgulanır; `data.max_usd_per_pull`
//! aşılırsa indirme durur ve onay ister.
//!
//! Durum: API anahtarı yok (M0). Maliyet sorgusu ve kapı hazır; DBN indirme anahtar gelince
//! `dbn` crate'iyle eklenecek (docs/OPEN_QUESTIONS.md).

use atlas_core::Money;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub dataset: String,
    pub symbols: String,
    pub stype_in: String,
    pub schema: String,
    /// RFC3339 / ISO tarih.
    pub start: String,
    pub end: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CostError {
    #[error(
        "maliyet {quote} tavanı {cap} aşıyor — onay gerekli (atlas.toml data.max_usd_per_pull)"
    )]
    NeedsApproval { quote: Money, cap: Money },
    #[error("ATLAS_DATABENTO_KEY tanımlı değil")]
    NoKey,
    #[error("databento: {0}")]
    Api(String),
}

pub trait CostQuoter {
    fn quote(&self, req: &PullRequest) -> Result<Money, CostError>;
}

/// Tavanı aşan çekim reddedilir. Aynen eşitlik serbest.
pub fn guard(quote: Money, cap: Money) -> Result<Money, CostError> {
    if quote > cap {
        Err(CostError::NeedsApproval { quote, cap })
    } else {
        Ok(quote)
    }
}

pub struct DatabentoQuoter {
    key: String,
    client: reqwest::blocking::Client,
}

impl DatabentoQuoter {
    pub fn from_env() -> Result<Self, CostError> {
        let key = std::env::var("ATLAS_DATABENTO_KEY").map_err(|_| CostError::NoKey)?;
        if key.trim().is_empty() {
            return Err(CostError::NoKey);
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| CostError::Api(e.to_string()))?;
        Ok(DatabentoQuoter { key, client })
    }
}

impl CostQuoter for DatabentoQuoter {
    fn quote(&self, r: &PullRequest) -> Result<Money, CostError> {
        let resp = self
            .client
            .post("https://hist.databento.com/v0/metadata.get_cost")
            .basic_auth(&self.key, Some(""))
            .form(&[
                ("dataset", r.dataset.as_str()),
                ("symbols", r.symbols.as_str()),
                ("stype_in", r.stype_in.as_str()),
                ("schema", r.schema.as_str()),
                ("start", r.start.as_str()),
                ("end", r.end.as_str()),
            ])
            .send()
            .map_err(|e| CostError::Api(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| CostError::Api(e.to_string()))?;
        if !status.is_success() {
            return Err(CostError::Api(format!("{status}: {text}")));
        }
        // Yanıt ondalık USD sayısıdır; metin olarak tamsayıya çevrilir (f64 yok).
        Money::parse_usd(text.trim()).map_err(|e| CostError::Api(format!("{e}: {text}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Money);
    impl CostQuoter for Fixed {
        fn quote(&self, _: &PullRequest) -> Result<Money, CostError> {
            Ok(self.0)
        }
    }

    #[test]
    fn guard_blocks_over_cap() {
        let req = PullRequest {
            dataset: "GLBX.MDP3".into(),
            symbols: "NQ.c.0".into(),
            stype_in: "continuous".into(),
            schema: "ohlcv-1m".into(),
            start: "2024-01-01".into(),
            end: "2024-02-01".into(),
        };
        let q = Fixed(Money::parse_usd("12.34").unwrap())
            .quote(&req)
            .unwrap();
        assert!(guard(q, Money::from_usd(20)).is_ok());
        assert!(guard(q, Money::from_usd(12)).is_err());
        assert!(guard(Money::from_usd(5), Money::from_usd(5)).is_ok());
    }
}
