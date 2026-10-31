//! ATLAS_HOME ve yapılandırma yükleme.

use anyhow::{Context, Result, anyhow};
use atlas_core::{Calendar, InstrumentTable, Money};
use atlas_risk::{ConfigError, PropRules, RiskPolicy};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct AtlasToml {
    pub paths: Paths,
    pub account: Account,
    pub data: DataCfg,
    #[serde(default)]
    pub verify: VerifyCfg,
}

#[derive(Debug, Deserialize)]
pub struct Paths {
    pub journal: String,
    pub store: String,
    #[serde(default = "default_registry")]
    pub registry: String,
}

fn default_registry() -> String {
    "var/registry.sqlite".into()
}

#[derive(Debug, Deserialize)]
pub struct Account {
    pub prop: String,
}

#[derive(Debug, Deserialize)]
pub struct DataCfg {
    #[serde(default)]
    pub max_usd_per_pull: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyCfg {
    #[serde(default = "default_seed")]
    pub seed: u64,
}

impl Default for VerifyCfg {
    fn default() -> Self {
        VerifyCfg {
            seed: default_seed(),
        }
    }
}

fn default_seed() -> u64 {
    20261031
}

pub struct Home {
    pub root: PathBuf,
    pub cfg: AtlasToml,
    pub atlas_toml_text: String,
}

impl Home {
    pub fn locate(explicit: Option<PathBuf>) -> Result<Home> {
        let root = match explicit.or_else(|| std::env::var_os("ATLAS_HOME").map(PathBuf::from)) {
            Some(p) => p,
            None => std::env::current_dir()?,
        };
        let p = root.join("configs").join("atlas.toml");
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("{} okunamadı (ATLAS_HOME doğru mu?)", p.display()))?;
        let cfg: AtlasToml =
            toml::from_str(&text).with_context(|| format!("{} çözülemedi", p.display()))?;
        Ok(Home {
            root,
            cfg,
            atlas_toml_text: text,
        })
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        let p = Path::new(rel);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    pub fn configs(&self) -> PathBuf {
        self.root.join("configs")
    }

    pub fn journal_path(&self) -> PathBuf {
        self.path(&self.cfg.paths.journal)
    }

    pub fn store_path(&self) -> PathBuf {
        self.path(&self.cfg.paths.store)
    }

    pub fn instruments(&self) -> Result<InstrumentTable> {
        let p = self.configs().join("instruments.toml");
        let t = std::fs::read_to_string(&p).with_context(|| p.display().to_string())?;
        InstrumentTable::from_toml_str(&t).map_err(|e| anyhow!("instruments.toml: {e}"))
    }

    pub fn calendar(&self) -> Result<Calendar> {
        let p = self.configs().join("calendar.toml");
        let t = std::fs::read_to_string(&p).with_context(|| p.display().to_string())?;
        Calendar::from_toml_str(&t).map_err(|e| anyhow!("calendar.toml: {e}"))
    }

    pub fn policy(&self) -> Result<RiskPolicy> {
        RiskPolicy::from_toml_str(&self.atlas_toml_text)
            .map_err(|e| anyhow!("atlas.toml [risk]: {e}"))
    }

    pub fn prop_path(&self) -> PathBuf {
        self.configs().join(&self.cfg.account.prop)
    }

    /// Prop config'i yükler. Boş alan hatası ayrı döner (Arman doldurmalı).
    pub fn prop(&self) -> Result<std::result::Result<PropRules, ConfigError>> {
        let p = self.prop_path();
        let t = std::fs::read_to_string(&p).with_context(|| p.display().to_string())?;
        Ok(PropRules::from_toml_str(&t))
    }

    /// Databento çekim tavanı. Boş = 0 (her ücretli çekim onay ister).
    pub fn data_cap(&self) -> Result<Money> {
        let s = self.cfg.data.max_usd_per_pull.trim();
        if s.is_empty() {
            return Ok(Money::ZERO);
        }
        Money::parse_usd(s).map_err(|e| anyhow!("data.max_usd_per_pull: {e}"))
    }
}
