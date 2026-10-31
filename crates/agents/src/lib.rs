//! LLM ajanları (§4.8). Sağlayıcıdan bağımsız (`LlmProvider`); her çağrı token/USD olarak `risk`
//! bütçesinden düşer; bütçe biterse ajanlar durur, sistem mekanik çalışmaya devam eder.
//!
//! **LLM çıktısı güvenilmez girdidir** (kural 9): JSON şemasıyla (bilinmeyen alan reddi) ayrıştırılır,
//! anlamsal doğrulamadan geçer; geçersizse atılır ve sayılır. Ajanlar `risk`'e, `journal` yazımına
//! (not hariç), canlı config'e ve sermayeye dokunamaz: bu crate bunlara bağımlı değildir; yalnız
//! sicil kuyruğuna aday bırakır ve öneri dosyası yazar.
//!
//! Roller (ayrı, sürümlü prompt dosyaları `prompts/*.md`):
//! - Araştırmacı → hipotez (gerekçe, genom TOML, beklenen davranış, çürütme kriteri)
//! - Eleştirmen → "neden sahte olabilir" + kontrol deneyi; onay olmadan aday kuyruğa girmez
//! - Mühendis → plugin/platform önerisi (dosyaya; merge insan işi)
//! - Raporcu → Türkçe özet (bütçe yoksa deterministik şablon)

pub mod provider;

use atlas_core::Money;
use atlas_genome::Genome;
use atlas_registry::Registry;
use atlas_risk::BudgetTracker;
use chrono::NaiveDate;
use provider::{LlmProvider, LlmResponse};
use serde::{Deserialize, Serialize};

pub const RESEARCHER_PROMPT: &str = include_str!("../prompts/researcher.v1.md");
pub const CRITIC_PROMPT: &str = include_str!("../prompts/critic.v1.md");
pub const ENGINEER_PROMPT: &str = include_str!("../prompts/engineer.v1.md");
pub const REPORTER_PROMPT: &str = include_str!("../prompts/reporter.v1.md");
pub const PROMPT_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hypothesis {
    pub rationale: String,
    pub genome_toml: String,
    pub expected_behavior: String,
    pub falsification: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Critique {
    /// "onay" | "ret"
    pub verdict: String,
    pub why_fake: String,
    pub control_experiment: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineerProposal {
    pub title: String,
    pub rationale: String,
    /// "plugin" | "platform"
    pub kind: String,
    pub content: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("bütçe: {0}")]
    Budget(#[from] atlas_risk::budget::BudgetExhausted),
    #[error("sağlayıcı: {0}")]
    Provider(String),
    #[error("geçersiz çıktı ({role}): {reason}")]
    Invalid { role: &'static str, reason: String },
    #[error("sicil: {0}")]
    Registry(#[from] atlas_registry::RegistryError),
}

/// Metindeki ilk dengeli `{...}` JSON nesnesini çıkarır (model açıklama eklese bile).
pub fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in text[start..].char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn parse<T: for<'de> Deserialize<'de>>(
    role: &'static str,
    text: &str,
) -> Result<T, AgentError> {
    let j = extract_json(text).ok_or(AgentError::Invalid {
        role,
        reason: "JSON nesnesi yok".into(),
    })?;
    serde_json::from_str(j).map_err(|e| AgentError::Invalid {
        role,
        reason: e.to_string(),
    })
}

/// Hipotezin genomunu doğrular; enstrüman ve risk sistem tarafından zorlanır (ajan değiştiremez).
pub fn validate_hypothesis(
    h: &Hypothesis,
    instrument: &str,
    risk_usd: u32,
) -> Result<Genome, AgentError> {
    let inv = |r: String| AgentError::Invalid {
        role: "araştırmacı",
        reason: r,
    };
    for (n, f, max) in [
        ("rationale", &h.rationale, 4000),
        ("expected_behavior", &h.expected_behavior, 2000),
        ("falsification", &h.falsification, 2000),
        ("genome_toml", &h.genome_toml, 8000),
    ] {
        if f.trim().is_empty() || f.chars().count() > max {
            return Err(inv(format!("{n} boş ya da çok uzun")));
        }
    }
    let mut g = Genome::from_toml_str(&h.genome_toml).map_err(|e| inv(e.to_string()))?;
    g.instrument = instrument.to_string();
    g.sizing.risk_usd = risk_usd;
    g.name = format!(
        "ajan-{}",
        g.name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(40)
            .collect::<String>()
    );
    g.tags = vec!["ajan".into()];
    g.validate().map_err(|e| inv(e.to_string()))?;
    Ok(g)
}

pub fn validate_critique(c: &Critique) -> Result<bool, AgentError> {
    match c.verdict.trim() {
        "onay" if !c.why_fake.trim().is_empty() && !c.control_experiment.trim().is_empty() => {
            Ok(true)
        }
        "ret" => Ok(false),
        _ => Err(AgentError::Invalid {
            role: "eleştirmen",
            reason: "verdict onay|ret ve gerekçeler zorunlu".into(),
        }),
    }
}

/// Fiyat (1M token başına USD, mikro-dolar). `None` = doldurulmadı → çağrı yapılmaz (uydurma yok).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    pub input_per_mtok: Money,
    pub output_per_mtok: Money,
}

impl Pricing {
    pub fn cost(&self, r: &LlmResponse) -> Money {
        let i = i128::from(self.input_per_mtok.0) * i128::from(r.input_tokens);
        let o = i128::from(self.output_per_mtok.0) * i128::from(r.output_tokens);
        Money(i64::try_from((i + o + 999_999) / 1_000_000).unwrap_or(i64::MAX))
    }
}

/// Bütçeli çağrı: önce izin, sonra çağrı, sonra harcama.
pub struct Budgeted<'a> {
    pub provider: &'a dyn LlmProvider,
    pub pricing: Pricing,
    pub budget: &'a mut BudgetTracker,
    pub day: NaiveDate,
    pub now: atlas_core::UnixNanos,
    pub events: Vec<atlas_core::Event>,
}

impl Budgeted<'_> {
    pub fn call(
        &mut self,
        role: &'static str,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<String, AgentError> {
        self.budget.can_spend(self.day)?;
        let r = self
            .provider
            .complete(system, user, max_tokens)
            .map_err(AgentError::Provider)?;
        let usd = self.pricing.cost(&r);
        self.events.push(atlas_core::Event::new(
            self.now,
            atlas_core::EventKind::AgentCall,
            serde_json::json!({"role": role, "provider": self.provider.name(), "prompt": PROMPT_VERSION,
                "input_tokens": r.input_tokens, "output_tokens": r.output_tokens, "usd": usd}),
        ));
        if let Some(e) =
            self.budget
                .charge(self.day, self.now, usd, r.input_tokens + r.output_tokens)
        {
            self.events.push(e);
        }
        Ok(r.text)
    }

    fn invalid(&mut self, e: &AgentError) {
        self.events.push(atlas_core::Event::new(
            self.now,
            atlas_core::EventKind::AgentInvalidOutput,
            serde_json::json!({"error": e.to_string()}),
        ));
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ResearchReport {
    pub hypotheses: usize,
    pub invalid: usize,
    pub rejected_by_critic: usize,
    pub rejected_dead_idea: usize,
    pub queued: Vec<String>,
}

/// Araştırmacı → (bilgi tabanı) → eleştirmen → kuyruk.
pub fn research_cycle(
    b: &mut Budgeted<'_>,
    reg: &mut Registry,
    context: &str,
    instrument: &str,
    risk_usd: u32,
    n: usize,
) -> Result<ResearchReport, AgentError> {
    let mut rep = ResearchReport::default();
    for _ in 0..n {
        let text = b.call("araştırmacı", RESEARCHER_PROMPT, context, 1500)?;
        rep.hypotheses += 1;
        let h = match parse::<Hypothesis>("araştırmacı", &text) {
            Ok(h) => h,
            Err(e) => {
                b.invalid(&e);
                rep.invalid += 1;
                continue;
            }
        };
        let g = match validate_hypothesis(&h, instrument, risk_usd) {
            Ok(g) => g,
            Err(e) => {
                b.invalid(&e);
                rep.invalid += 1;
                continue;
            }
        };
        if !reg.similar_dead(&g.tokens(), &g.tags, 0.10)?.is_empty() {
            rep.rejected_dead_idea += 1;
            continue;
        }
        let critic_in = format!(
            "Hipotez:\n{}\n\nGenom:\n{}\n\nBeklenen davranış: {}\nÇürütme kriteri: {}",
            h.rationale,
            g.to_toml(),
            h.expected_behavior,
            h.falsification
        );
        let ctext = b.call("eleştirmen", CRITIC_PROMPT, &critic_in, 800)?;
        let ok = match parse::<Critique>("eleştirmen", &ctext).and_then(|c| validate_critique(&c))
        {
            Ok(v) => v,
            Err(e) => {
                b.invalid(&e);
                rep.invalid += 1;
                continue;
            }
        };
        if !ok {
            rep.rejected_by_critic += 1;
            continue;
        }
        reg.queue_genome(
            &serde_json::to_string(&g).unwrap_or_default(),
            &format!("ajan:araştırmacı/{PROMPT_VERSION}"),
            &h.rationale.chars().take(500).collect::<String>(),
            b.now.0,
        )?;
        rep.queued.push(g.name.clone());
    }
    Ok(rep)
}

/// Mühendis önerisi: yalnız dosyaya yazılır (`dir/<zaman>-<başlık>.json`); merge insan işidir.
pub fn engineer_proposal(
    b: &mut Budgeted<'_>,
    context: &str,
    dir: &std::path::Path,
) -> Result<std::path::PathBuf, AgentError> {
    let text = b.call("mühendis", ENGINEER_PROMPT, context, 4000)?;
    let p: EngineerProposal = parse("mühendis", &text).inspect_err(|e| b.invalid(e))?;
    if !matches!(p.kind.as_str(), "plugin" | "platform") || p.title.trim().is_empty() {
        let e = AgentError::Invalid {
            role: "mühendis",
            reason: "kind plugin|platform, başlık zorunlu".into(),
        };
        b.invalid(&e);
        return Err(e);
    }
    std::fs::create_dir_all(dir).map_err(|e| AgentError::Provider(e.to_string()))?;
    let safe: String = p
        .title
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect();
    let path = dir.join(format!("{}-{safe}.json", b.now.0));
    std::fs::write(&path, serde_json::to_vec_pretty(&p).unwrap_or_default())
        .map_err(|e| AgentError::Provider(e.to_string()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_risk::LlmBudget;
    use provider::MockProvider;

    const GOOD_H: &str = r#"Elbette, hipotezim şu:
{"rationale":"Trend günlerinde geri çekilmeler devam eder.","genome_toml":"name = \"tp\"\ninstrument = \"X\"\n[entry]\nlong = { all = [ { gt = [\"ema:50\", \"ema:200\"] }, { lt = [\"low\", \"ema:50\"] } ] }\n[exit]\natr = 14\nstop_atr = 3.0\ntarget_r = 2.0\ntime_bars = 600\n[sizing]\nrisk_usd = 999999\n","expected_behavior":"Trendde pozitif","falsification":"CPCV medyanı ≤ 0"}"#;
    const OK_C: &str = r#"{"verdict":"onay","why_fake":"Trend dönemine aşırı uyum olabilir","control_experiment":"Rastgele giriş, aynı çıkışlar"}"#;

    fn tracker(usd: i64) -> BudgetTracker {
        BudgetTracker::new(LlmBudget {
            daily_usd: Money::from_usd(usd),
            daily_tokens: 1_000_000,
        })
    }

    fn pricing() -> Pricing {
        Pricing {
            input_per_mtok: Money::from_usd(3),
            output_per_mtok: Money::from_usd(15),
        }
    }

    #[test]
    fn pipeline_queues_valid_and_rejects_invalid() {
        let mut reg = Registry::open_in_memory().unwrap();
        let p = MockProvider::new(vec![
            GOOD_H.into(),
            OK_C.into(),
            "geçersiz çıktı".into(),
            r#"{"rationale":"x","genome_toml":"bozuk","expected_behavior":"y","falsification":"z"}"#.into(),
            r#"{"rationale":"x","genome_toml":"","expected_behavior":"y","falsification":"z","ekstra":1}"#.into(),
        ]);
        let mut bt = tracker(10);
        let mut b = Budgeted {
            provider: &p,
            pricing: pricing(),
            budget: &mut bt,
            day: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            now: atlas_core::UnixNanos(1),
            events: vec![],
        };
        let r = research_cycle(&mut b, &mut reg, "bağlam", "BTCUSDT", 1000, 4).unwrap();
        assert_eq!(r.queued.len(), 1);
        assert_eq!(r.invalid, 3);
        let q = reg.take_queued(2).unwrap().unwrap();
        let g: Genome = serde_json::from_str(&q.genome_json).unwrap();
        // Enstrüman ve risk ajan tarafından belirlenemez.
        assert_eq!(g.instrument, "BTCUSDT");
        assert_eq!(g.sizing.risk_usd, 1000);
        assert!(q.source.starts_with("ajan:"));
        assert_eq!(
            b.events
                .iter()
                .filter(|e| e.kind == atlas_core::EventKind::AgentInvalidOutput)
                .count(),
            3
        );
    }

    #[test]
    fn critic_reject_and_budget_stop() {
        let mut reg = Registry::open_in_memory().unwrap();
        let p = MockProvider::new(vec![
            GOOD_H.into(),
            r#"{"verdict":"ret","why_fake":"veri madenciliği","control_experiment":"yok"}"#.into(),
        ]);
        let mut bt = tracker(10);
        let mut b = Budgeted {
            provider: &p,
            pricing: pricing(),
            budget: &mut bt,
            day: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            now: atlas_core::UnixNanos(1),
            events: vec![],
        };
        let r = research_cycle(&mut b, &mut reg, "c", "BTCUSDT", 1000, 1).unwrap();
        assert_eq!(r.rejected_by_critic, 1);
        assert!(reg.take_queued(1).unwrap().is_none());
        // Bütçe 0 → çağrı yapılmaz, sistem durmaz (hata döner, çağıran mekanik devam eder).
        let mut z = tracker(0);
        let mut b0 = Budgeted {
            provider: &p,
            pricing: pricing(),
            budget: &mut z,
            day: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            now: atlas_core::UnixNanos(1),
            events: vec![],
        };
        assert!(matches!(
            research_cycle(&mut b0, &mut reg, "c", "B", 1, 1),
            Err(AgentError::Budget(_))
        ));
        assert_eq!(p.calls(), 2);
    }

    #[test]
    fn extract_json_handles_nesting_and_strings() {
        assert_eq!(
            extract_json(r#"x {"a":{"b":"}"}} y"#),
            Some(r#"{"a":{"b":"}"}}"#)
        );
        assert_eq!(extract_json("yok"), None);
    }

    #[test]
    fn pricing_rounds_up() {
        let r = LlmResponse {
            text: String::new(),
            input_tokens: 1000,
            output_tokens: 1000,
        };
        // 1000×$3/M + 1000×$15/M = $0.018
        assert_eq!(pricing().cost(&r), Money(18_000));
    }
}
