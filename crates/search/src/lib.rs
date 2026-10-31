//! Arama (§4.9, §4.11 L2): evrimsel döngü → hızlı tarama (BarFill, kısa dönem) → elit → tam gauntlet.
//!
//! - **Hızlı taramadaki her değerlendirme sicile yazılır** (deneme sayısına girer; kapı 0 = tarama).
//! - Kaynaklar: rastgele, mutasyon, çaprazlama, Bayes (TPE benzeri), ajan kuyruğu, insan. Bir sonraki
//!   adayın kaynağı **meta-öğrenme** ile seçilir: kaynak başına Beta(1+başarı, 1+başarısızlık) Thompson
//!   örneklemesi; başarı = taramayı geçip gauntlet'e girmek (kapı ≥ 2'ye ulaşmak).
//! - Bilgi tabanı: yapısal olarak çok benzer ölü fikir (mesafe < eşik) yeniden denenmez.

use atlas_core::UnixNanos;
use atlas_gauntlet::eval::{Market, nets};
use atlas_gauntlet::{Context, GauntletReport, stats};
use atlas_genome::Genome;
use atlas_genome::evolve::{crossover, mutate, params, random_genome, set_param};
use atlas_registry::ladder::Stage;
use atlas_registry::{ExperimentRecord, Registry};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Hızlı tarama dönemi (in-sample'ın son N günü).
    pub screen_days: u64,
    pub min_screen_trades: usize,
    /// Tam gauntlet'e gönderme eşiği: tarama t-istatistiği.
    pub screen_t_min: f64,
    pub population: usize,
    pub evals_per_cycle: usize,
    pub risk_usd: u32,
    /// Yapısal mesafe bu değerin altındaysa "ölü fikrin tekrarı" sayılır.
    pub dead_idea_distance: f64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            screen_days: 60,
            min_screen_trades: 60,
            screen_t_min: 2.0,
            population: 40,
            evals_per_cycle: 4,
            risk_usd: 1000,
            dead_idea_distance: 0.05,
        }
    }
}

pub const SOURCES: &[&str] = &[
    "evrim:rastgele",
    "evrim:mutasyon",
    "evrim:çaprazlama",
    "bayes",
];

#[derive(Debug, Clone, Serialize)]
pub struct Screen {
    pub trades: usize,
    pub mean: f64,
    pub t: f64,
    pub sharpe: f64,
}

#[derive(Debug, Clone, Serialize)]
struct Scored {
    genome: Genome,
    t: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CycleReport {
    pub screened: usize,
    pub skipped_dead_ideas: usize,
    pub sent_to_gauntlet: usize,
    pub gauntlet_passed: Vec<String>,
    pub by_source: BTreeMap<String, usize>,
    pub best_t: f64,
}

pub struct Searcher {
    rng: ChaCha8Rng,
    pub cfg: SearchConfig,
    instrument: String,
    pop: Vec<Scored>,
    /// Kaynak → (başarı, başarısızlık)
    beta: BTreeMap<String, (f64, f64)>,
    /// Bayes geçmişi: temel genom hash'i → (parametre oranları, skor)
    bayes: BTreeMap<String, Vec<(Vec<f64>, f64)>>,
    counter: u64,
}

pub fn screen(
    ctx: &Context<'_>,
    short: &Market,
    g: &Genome,
) -> Result<Screen, atlas_gauntlet::eval::EvalError> {
    let t = ctx.eval.run(g, short, ctx.cost, None)?;
    let x = nets(&t);
    let sd = stats::sd(&x);
    let mean = stats::mean(&x);
    let tstat = if sd > 0.0 {
        mean / sd * (x.len() as f64).sqrt()
    } else {
        0.0
    };
    Ok(Screen {
        trades: x.len(),
        mean,
        t: tstat,
        sharpe: stats::sharpe(&x),
    })
}

/// Kısa tarama piyasası: in-sample'ın son `days` günü.
pub fn screen_market(m: &Market, days: u64) -> Market {
    let last = m.bars.last().map_or(UnixNanos::ZERO, |b| b.close_ts);
    let from = last.minus(days * atlas_core::units::NANOS_PER_DAY);
    let cut = m.bars.partition_point(|b| b.open_ts < from);
    Market {
        data_version: format!("{}-screen{days}", m.data_version),
        bars: std::sync::Arc::new(m.bars[cut..].to_vec()),
        ..m.clone()
    }
}

impl Searcher {
    pub fn new(seed: u64, instrument: &str, cfg: SearchConfig, seeds: Vec<Genome>) -> Searcher {
        Searcher {
            rng: ChaCha8Rng::seed_from_u64(seed),
            cfg,
            instrument: instrument.into(),
            pop: seeds
                .into_iter()
                .map(|g| Scored { genome: g, t: 0.0 })
                .collect(),
            beta: SOURCES
                .iter()
                .map(|s| (s.to_string(), (1.0, 1.0)))
                .collect(),
            bayes: BTreeMap::new(),
            counter: 0,
        }
    }

    /// Sicildeki geçmişten kaynak başarılarını yükler (meta-öğrenme süreklidir).
    pub fn load_source_priors(
        &mut self,
        reg: &Registry,
    ) -> Result<(), atlas_registry::RegistryError> {
        for (src, n, reached) in reg.source_progress()? {
            if let Some(b) = self.beta.get_mut(&src) {
                *b = (1.0 + reached as f64, 1.0 + (n - reached) as f64);
            }
        }
        Ok(())
    }

    pub fn source_weights(&self) -> BTreeMap<String, f64> {
        self.beta
            .iter()
            .map(|(k, (a, b))| (k.clone(), a / (a + b)))
            .collect()
    }

    fn beta_sample(&mut self, a: f64, b: f64) -> f64 {
        // Gamma(a)/(Gamma(a)+Gamma(b)) yerine: ortalama + gürültü (a,b küçükken geniş) — deterministik ve yeterli.
        let m = a / (a + b);
        let sd = (a * b / ((a + b).powi(2) * (a + b + 1.0))).sqrt();
        let z = (0..12).map(|_| self.rng.random::<f64>()).sum::<f64>() - 6.0;
        (m + sd * z).clamp(0.0, 1.0)
    }

    fn pick_source(&mut self) -> String {
        let keys: Vec<(String, f64, f64)> = self
            .beta
            .iter()
            .map(|(k, (a, b))| (k.clone(), *a, *b))
            .collect();
        let mut best = (String::new(), f64::MIN);
        for (k, a, b) in keys {
            let s = self.beta_sample(a, b);
            if s > best.1 {
                best = (k, s);
            }
        }
        best.0
    }

    fn elite(&mut self) -> Option<Genome> {
        if self.pop.is_empty() {
            return None;
        }
        // Turnuva seçimi (3).
        let n = self.pop.len();
        let mut best: Option<&Scored> = None;
        for _ in 0..3 {
            let c = &self.pop[self.rng.random_range(0..n)];
            if best.is_none_or(|b| c.t > b.t) {
                best = Some(c);
            }
        }
        best.map(|s| s.genome.clone())
    }

    fn propose_bayes(&mut self, base: &Genome) -> Genome {
        let ps = params(base);
        let hist = self.bayes.get(&base.hash()).cloned().unwrap_or_default();
        if hist.len() < 8 || ps.is_empty() {
            return mutate(base, &mut self.rng);
        }
        let mut sorted = hist.clone();
        sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
        let k = (sorted.len() / 4).max(2);
        let (good, bad) = sorted.split_at(k);
        let dens = |x: &[f64], set: &[(Vec<f64>, f64)]| -> f64 {
            set.iter()
                .map(|(p, _)| {
                    let d2: f64 = p.iter().zip(x).map(|(a, b)| (a - b).powi(2)).sum();
                    (-d2 / (2.0 * 0.1 * 0.1)).exp()
                })
                .sum::<f64>()
                / set.len().max(1) as f64
                + 1e-12
        };
        let mut best: Option<(Vec<f64>, f64)> = None;
        for _ in 0..24 {
            let g = &good[self.rng.random_range(0..good.len())].0;
            let cand: Vec<f64> = g
                .iter()
                .map(|v| {
                    let z = (0..12).map(|_| self.rng.random::<f64>()).sum::<f64>() - 6.0;
                    (v + 0.1 * z).clamp(0.3, 3.0)
                })
                .collect();
            let score = dens(&cand, good) / dens(&cand, bad);
            if best.as_ref().is_none_or(|b| score > b.1) {
                best = Some((cand, score));
            }
        }
        let ratios = best.map_or_else(|| vec![1.0; ps.len()], |b| b.0);
        let mut g = base.clone();
        for (i, (p, r)) in ps.iter().zip(&ratios).enumerate() {
            let v = if p.value == 0.0 { r - 1.0 } else { p.value * r };
            g = set_param(&g, i, v);
        }
        g
    }

    fn ratios(base: &Genome, g: &Genome) -> Vec<f64> {
        params(base)
            .iter()
            .zip(params(g))
            .map(|(a, b)| {
                if a.value == 0.0 {
                    b.value + 1.0
                } else {
                    b.value / a.value
                }
            })
            .collect()
    }

    fn candidate(&mut self, source: &str) -> (Genome, Option<String>) {
        self.counter += 1;
        let name = format!("ara-{}-{:06}", source.replace(':', "-"), self.counter);
        let risk = self.cfg.risk_usd;
        let inst = self.instrument.clone();
        let (mut g, base) = match source {
            "evrim:mutasyon" => match self.elite() {
                Some(e) => (mutate(&e, &mut self.rng), None),
                None => (random_genome(&mut self.rng, &inst, &name, risk), None),
            },
            "evrim:çaprazlama" => match (self.elite(), self.elite()) {
                (Some(a), Some(b)) => (crossover(&a, &b, &mut self.rng), None),
                _ => (random_genome(&mut self.rng, &inst, &name, risk), None),
            },
            "bayes" => match self.elite() {
                Some(e) => {
                    let h = e.hash();
                    (self.propose_bayes(&e), Some(h))
                }
                None => (random_genome(&mut self.rng, &inst, &name, risk), None),
            },
            _ => (random_genome(&mut self.rng, &inst, &name, risk), None),
        };
        g.name = name;
        g.tags = vec!["arama".into(), source.into()];
        (g, base)
    }

    /// Bir arama döngüsü. Kuyruktaki (ajan/insan) genomlar önceliklidir.
    pub fn cycle(
        &mut self,
        ctx: &Context<'_>,
        short: &Market,
        reg: &mut Registry,
        now: UnixNanos,
    ) -> Result<(CycleReport, Vec<GauntletReport>), SearchError> {
        let mut rep = CycleReport::default();
        let mut reports = Vec::new();
        for _ in 0..self.cfg.evals_per_cycle {
            let (g, source, base) = match reg.take_queued(now.0)? {
                Some(q) => match serde_json::from_str::<Genome>(&q.genome_json) {
                    Ok(g) if g.validate().is_ok() => (g, q.source, None),
                    _ => continue,
                },
                None => {
                    let s = self.pick_source();
                    let (g, base) = self.candidate(&s);
                    (g, s, base)
                }
            };
            *rep.by_source.entry(source.clone()).or_default() += 1;
            let similar = reg.similar_dead(&g.tokens(), &g.tags, self.cfg.dead_idea_distance)?;
            if similar.iter().any(|(l, _)| l.genome_hash == g.hash())
                || !similar.is_empty() && source.starts_with("evrim")
            {
                rep.skipped_dead_ideas += 1;
                continue;
            }
            let t0 = std::time::Instant::now();
            let sc = screen(ctx, short, &g)?;
            rep.screened += 1;
            rep.best_t = rep.best_t.max(sc.t);
            reg.record(&ExperimentRecord {
                parent_id: None,
                source: source.clone(),
                name: g.name.clone(),
                genome_hash: g.hash(),
                genome_json: serde_json::to_string(&g).unwrap_or_default(),
                tags: g.tags.clone(),
                data_version: short.data_version.clone(),
                fill_model: "bar_fill@v1".into(),
                seed: ctx.seed,
                passed: false,
                failed_gate: Some(0),
                trades: sc.trades as u64,
                sharpe: (sc.trades >= 2).then_some(sc.sharpe),
                report_json: serde_json::json!({"quick_screen": sc}).to_string(),
                report_md: format!(
                    "hızlı tarama: {} işlem, t={:.2}, ort ${:.2}",
                    sc.trades, sc.t, sc.mean
                ),
                cpu_ms: t0.elapsed().as_millis() as u64,
                llm_tokens: 0,
                created_ts: now.0,
            })?;
            if let Some(b) = &base {
                let r = Self::ratios(
                    &self
                        .pop
                        .iter()
                        .find(|s| &s.genome.hash() == b)
                        .map_or(g.clone(), |s| s.genome.clone()),
                    &g,
                );
                self.bayes.entry(b.clone()).or_default().push((r, sc.t));
            }
            self.pop.push(Scored {
                genome: g.clone(),
                t: sc.t,
            });
            self.pop.sort_by(|a, b| b.t.total_cmp(&a.t));
            self.pop.truncate(self.cfg.population);
            let promising =
                sc.trades >= self.cfg.min_screen_trades && sc.t >= self.cfg.screen_t_min;
            if let Some(b) = self.beta.get_mut(&source) {
                if promising { b.0 += 1.0 } else { b.1 += 1.0 }
            }
            if promising {
                rep.sent_to_gauntlet += 1;
                let (gr, _) = atlas_gauntlet::run(ctx, &g, reg, &source, None, now)?;
                if gr.passed {
                    reg.set_stage(
                        &gr.genome_hash,
                        &g.name,
                        Stage::GauntletPassed,
                        "gauntlet 10/10",
                        "sistem",
                        now.0,
                    )?;
                    reg.set_stage(
                        &gr.genome_hash,
                        &g.name,
                        Stage::Shadow,
                        "otomatik terfi: gauntlet → gölge",
                        "sistem",
                        now.0,
                    )?;
                    rep.gauntlet_passed.push(g.name.clone());
                }
                reports.push(gr);
            }
        }
        Ok((rep, reports))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("sicil: {0}")]
    Registry(#[from] atlas_registry::RegistryError),
    #[error("değerlendirme: {0}")]
    Eval(#[from] atlas_gauntlet::eval::EvalError),
    #[error("gauntlet: {0}")]
    Gauntlet(#[from] atlas_gauntlet::GauntletError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::{InstrumentTable, SessionSpec};
    use atlas_features::FeatureStore;
    use atlas_gauntlet::GauntletConfig;
    use atlas_gauntlet::eval::{Evaluator, always_calendar, research_env};
    use atlas_gauntlet::holdout::split;
    use atlas_risk::PropRules;
    use std::sync::Arc;

    #[test]
    fn cycle_records_every_screen_and_is_deterministic() {
        let table = InstrumentTable::from_toml_str(
            "[[instrument]]\nsymbol=\"SYN\"\nvenue=\"binance\"\nkind=\"future\"\ntick_size=\"1\"\ntick_value_usd=\"1\"\nsession=\"always\"\n[instrument.costs]\ncommission_per_contract_usd=\"0.5\"\nslippage_ticks=\"1\"\n",
        )
        .unwrap();
        let bars = atlas_sim::synthetic::ar_bars(
            4,
            60_000,
            UnixNanos::from_secs(1_700_000_000),
            0.0,
            6.0,
            atlas_core::Price(100_000),
        );
        let (is_bars, vault) = split("SYN", bars, 10, 5);
        let m = Market {
            dataset: "SYN".into(),
            data_version: "t".into(),
            instrument: table.get("SYN").unwrap().clone(),
            session: SessionSpec::Always,
            bars: Arc::new(is_bars),
        };
        let short = screen_market(&m, 15);
        let store = FeatureStore::new();
        let ev = Evaluator::new(research_env(&table, &always_calendar()).unwrap(), &store);
        let prop = PropRules::from_toml_str(atlas_sim::validation::VALIDATION_PROP).unwrap();
        let cfg = GauntletConfig {
            mc_paths: 100,
            ..Default::default()
        };
        let ctx = Context {
            eval: &ev,
            market: &m,
            vault: &vault,
            prop: &prop,
            cfg: &cfg,
            seed: 1,
            cost: atlas_gauntlet::eval::Cost::BASE,
        };
        let run = || {
            let mut reg = Registry::open_in_memory().unwrap();
            reg.queue_genome(
                &serde_json::to_string(&atlas_genome::evolve::random_genome(
                    &mut atlas_genome::evolve::rng(9),
                    "SYN",
                    "kuyruk",
                    1000,
                ))
                .unwrap(),
                "ajan:researcher/v1",
                "test",
                0,
            )
            .unwrap();
            let mut s = Searcher::new(
                3,
                "SYN",
                SearchConfig {
                    evals_per_cycle: 6,
                    ..Default::default()
                },
                vec![],
            );
            let (r, _) = s.cycle(&ctx, &short, &mut reg, UnixNanos(1)).unwrap();
            (
                r.screened,
                reg.trial_count().unwrap(),
                r.by_source.clone(),
                s.source_weights(),
            )
        };
        let a = run();
        let b = run();
        // Her tarama sicilde (gauntlet'e giden adaylar ayrıca bir kayıt daha ekler).
        assert!(a.1 as usize >= a.0 && a.0 > 0, "{a:?}");
        assert_eq!(a.2.get("ajan:researcher/v1"), Some(&1));
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
}
