//! Gauntlet — sistemin çekirdeği (§4.6). Bir aday 10 kapıdan **sırayla** geçer; ilk düştüğü kapıda
//! durur, otopsi (ders) yazılır. Her koşu sicile kaydedilir (başarısızlar dahil, kural 7).
//!
//! | # | Kapı | Varsayılan kural |
//! |---|---|---|
//! | 1 | Asgari örneklem | ≥100 işlem, her CPCV bölmesinde test toplamı ≥30 |
//! | 2 | Purged+embargoed CPCV | test medyan beklenti > 0, bölmelerin ≥%60'ı pozitif |
//! | 3 | Walk-forward | ardışık her OOS penceresinde beklenti > 0 |
//! | 4 | Deflated Sharpe | sicildeki toplam deneme sayısıyla DSR ≥ 0,95 |
//! | 5 | PBO (CSCV) | ≤ 0,20 (parametre komşuluğu konfigürasyon kümesi) |
//! | 6 | Parametre stabilitesi | her parametrenin ±%20'sinde beklenti > 0 |
//! | 7 | Maliyet stresi | 2× kayma + 2× ücretle beklenti ≥ 0 |
//! | 8 | Rejim kırılımı | tek rejim toplam PnL'in > %70'i → düşer |
//! | 9 | Prop Monte Carlo | geçme ≥ eşik, patlama ≤ eşik (eşikler Arman'ın) |
//! | 10 | Holdout | kilitli son dönem, erişim bütçeli; beklenti > 0 |

pub mod eval;
pub mod fake;
pub mod holdout;
pub mod prop_mc;
pub mod stats;

use atlas_core::UnixNanos;
use atlas_core::units::NANOS_PER_DAY;
use atlas_features::FeatureSpec;
use atlas_genome::Genome;
use atlas_registry::{ExperimentRecord, Registry};
use atlas_risk::PropRules;
use atlas_sim::TradeRecord;
use eval::{Cost, EvalError, Evaluator, Market, nets};
use holdout::HoldoutVault;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GauntletConfig {
    pub min_trades_is: usize,
    pub min_trades_cpcv_test: usize,
    pub cpcv_groups: usize,
    pub cpcv_test_groups: usize,
    pub cpcv_min_positive: f64,
    pub embargo_frac: f64,
    pub wf_windows: usize,
    pub dsr_min: f64,
    pub pbo_max: f64,
    pub pbo_blocks: usize,
    pub stability_frac: f64,
    pub cost_stress_mult: f64,
    pub regime_max_share: f64,
    /// Arman'ın eşikleri (OPEN_QUESTIONS).
    pub prop_pass_min: f64,
    pub prop_bust_max: f64,
    pub mc_paths: usize,
    pub mc_horizon_days: usize,
    pub holdout_days: u64,
    pub holdout_budget: u64,
    pub holdout_min_trades: usize,
}

impl Default for GauntletConfig {
    fn default() -> Self {
        GauntletConfig {
            min_trades_is: 100,
            min_trades_cpcv_test: 30,
            cpcv_groups: 6,
            cpcv_test_groups: 2,
            cpcv_min_positive: 0.6,
            embargo_frac: 0.01,
            wf_windows: 4,
            dsr_min: 0.95,
            pbo_max: 0.20,
            pbo_blocks: 8,
            stability_frac: 0.20,
            cost_stress_mult: 2.0,
            regime_max_share: 0.70,
            prop_pass_min: 0.50,
            prop_bust_max: 0.20,
            mc_paths: 2000,
            mc_horizon_days: 120,
            holdout_days: 90,
            holdout_budget: 5,
            holdout_min_trades: 10,
        }
    }
}

impl GauntletConfig {
    /// `atlas.toml [gauntlet]` — yalnız **sıkılaştırma** kabul edilir (§4.6); prop eşikleri serbest.
    pub fn from_atlas_toml(text: &str) -> Result<GauntletConfig, String> {
        #[derive(Deserialize)]
        struct W {
            #[serde(default)]
            gauntlet: Option<GauntletConfig>,
        }
        let w: W = toml::from_str(text).map_err(|e| e.to_string())?;
        let c = w.gauntlet.unwrap_or_default();
        let d = GauntletConfig::default();
        let mut bad = Vec::new();
        let mut chk = |name: &str, ok: bool| {
            if !ok {
                bad.push(name.to_string());
            }
        };
        chk("min_trades_is", c.min_trades_is >= d.min_trades_is);
        chk(
            "min_trades_cpcv_test",
            c.min_trades_cpcv_test >= d.min_trades_cpcv_test,
        );
        chk(
            "cpcv_min_positive",
            c.cpcv_min_positive >= d.cpcv_min_positive,
        );
        chk("embargo_frac", c.embargo_frac >= d.embargo_frac);
        chk("wf_windows", c.wf_windows >= d.wf_windows);
        chk("dsr_min", c.dsr_min >= d.dsr_min);
        chk("pbo_max", c.pbo_max <= d.pbo_max);
        chk("stability_frac", c.stability_frac >= d.stability_frac);
        chk("cost_stress_mult", c.cost_stress_mult >= d.cost_stress_mult);
        chk("regime_max_share", c.regime_max_share <= d.regime_max_share);
        chk(
            "holdout_min_trades",
            c.holdout_min_trades >= d.holdout_min_trades,
        );
        chk(
            "cpcv_groups",
            c.cpcv_groups >= 4 && c.cpcv_test_groups >= 1 && c.cpcv_test_groups < c.cpcv_groups,
        );
        chk(
            "pbo_blocks",
            c.pbo_blocks >= 4 && c.pbo_blocks.is_multiple_of(2),
        );
        chk(
            "prop thresholds",
            (0.0..=1.0).contains(&c.prop_pass_min) && (0.0..=1.0).contains(&c.prop_bust_max),
        );
        if bad.is_empty() {
            Ok(c)
        } else {
            Err(format!(
                "[gauntlet] yalnız sıkılaştırılabilir; gevşek/geçersiz: {}",
                bad.join(", ")
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    pub gate: u32,
    pub name: String,
    pub pass: bool,
    pub metrics: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GauntletReport {
    pub genome_name: String,
    pub genome_hash: String,
    pub instrument: String,
    pub data_version: String,
    pub fill_model: String,
    pub trial_number: u64,
    pub trades_is: usize,
    pub expectancy_usd: f64,
    pub sharpe: f64,
    pub net_usd: f64,
    pub gates: Vec<GateResult>,
    pub passed: bool,
    pub failed_gate: Option<u32>,
}

impl GauntletReport {
    pub fn to_markdown(&self) -> String {
        let mut s = format!(
            "# Gauntlet: {}\n\n- genom: `{}`\n- enstrüman: {} · veri: `{}` · dolum: {}\n- deneme no: {}\n- in-sample: {} işlem, beklenti ${:.2}, Sharpe/işlem {:.4}, net ${:.2}\n- **sonuç: {}**\n\n| # | Kapı | Sonuç | Ölçüler |\n|---|---|---|---|\n",
            self.genome_name,
            &self.genome_hash[..self.genome_hash.len().min(16)],
            self.instrument,
            &self.data_version[..self.data_version.len().min(12)],
            self.fill_model,
            self.trial_number,
            self.trades_is,
            self.expectancy_usd,
            self.sharpe,
            self.net_usd,
            match self.failed_gate {
                None if self.passed => "GEÇTİ".to_string(),
                Some(g) => format!("kapı {g}'de düştü"),
                None => "—".to_string(),
            }
        );
        for g in &self.gates {
            let m = g.metrics.to_string();
            let m = if m.len() > 300 {
                format!(
                    "{}…",
                    &m[..m.char_indices().nth(300).map_or(m.len(), |x| x.0)]
                )
            } else {
                m
            };
            s += &format!(
                "| {} | {} | {} | `{}` |\n",
                g.gate,
                g.name,
                if g.pass { "✓" } else { "✗" },
                m.replace('|', "/")
            );
        }
        s
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GauntletError {
    #[error("{0}")]
    Eval(#[from] EvalError),
    #[error("sicil: {0}")]
    Registry(#[from] atlas_registry::RegistryError),
}

pub struct Context<'a> {
    pub eval: &'a Evaluator<'a>,
    /// In-sample veri (holdout hariç).
    pub market: &'a Market,
    pub vault: &'a HoldoutVault,
    pub prop: &'a PropRules,
    pub cfg: &'a GauntletConfig,
    pub seed: u64,
    /// Temel maliyet (normalde `Cost::BASE`). Sahte strateji testinin sürtünmesiz varyantı `ZERO` kullanır;
    /// kapı 7 bu tabanı `cost_stress_mult` ile çarpar.
    pub cost: Cost,
}

fn gate(n: u32, name: &str, pass: bool, metrics: serde_json::Value) -> GateResult {
    GateResult {
        gate: n,
        name: name.to_string(),
        pass,
        metrics,
    }
}

fn expectancy(t: &[TradeRecord]) -> f64 {
    stats::mean(&nets(t))
}

/// CPCV bölmeleri: (test grupları, purge/embargo sonrası test işlemleri).
fn cpcv_splits(
    t: &[TradeRecord],
    span: (UnixNanos, UnixNanos),
    cfg: &GauntletConfig,
) -> Vec<(Vec<usize>, Vec<f64>)> {
    let (a, b) = (span.0.0, span.1.0.max(span.0.0 + 1));
    let g = cfg.cpcv_groups as u64;
    let len = (b - a) / g;
    let bounds: Vec<(u64, u64)> = (0..g)
        .map(|i| (a + i * len, if i + 1 == g { b } else { a + (i + 1) * len }))
        .collect();
    let embargo = ((b - a) as f64 * cfg.embargo_frac) as u64;
    stats::combinations(cfg.cpcv_groups, cfg.cpcv_test_groups)
        .into_iter()
        .map(|test| {
            let xs = t
                .iter()
                .filter(|tr| {
                    test.iter().any(|&gi| {
                        let (lo, hi) = bounds[gi];
                        // Purge: işlem tamamen test grubunun içinde olmalı.
                        let inside = tr.entry_ts.0 >= lo && tr.exit_ts.0 < hi;
                        // Embargo: önceki grup eğitimdeyse test başından sonraki ilk dilim atılır.
                        let prev_train = gi > 0 && !test.contains(&(gi - 1));
                        inside && !(prev_train && tr.entry_ts.0 < lo + embargo)
                    })
                })
                .map(|tr| tr.net.as_usd_f64())
                .collect();
            (test, xs)
        })
        .collect()
}

fn regimes(ctx: &Context<'_>, t: &[TradeRecord]) -> BTreeMap<String, f64> {
    let atr: FeatureSpec = FeatureSpec::new(atlas_features::Kind::Atr, 60);
    let ema: FeatureSpec = FeatureSpec::new(atlas_features::Kind::Ema, 200);
    let close: FeatureSpec = FeatureSpec::new(atlas_features::Kind::Close, 0);
    let m = ctx.market;
    let cols = ctx
        .eval
        .store
        .columns(&m.data_version, &m.session, &m.bars, &[atr, ema, close]);
    let (Some(a), Some(e), Some(c)) = (
        cols.cols.get(&atr),
        cols.cols.get(&ema),
        cols.cols.get(&close),
    ) else {
        return BTreeMap::new();
    };
    let mut rel: Vec<f64> = a
        .iter()
        .zip(c.iter())
        .filter(|(x, y)| !x.is_nan() && **y > 0.0)
        .map(|(x, y)| x / y)
        .collect();
    rel.sort_by(f64::total_cmp);
    let med = rel.get(rel.len() / 2).copied().unwrap_or(0.0);
    let times = &cols.available_at;
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for tr in t {
        let i = times
            .partition_point(|x| *x <= tr.entry_ts)
            .saturating_sub(1);
        let (av, ev, cv) = (a[i], e[i], c[i]);
        let vol = if !av.is_nan() && cv > 0.0 && av / cv > med {
            "vol_yüksek"
        } else {
            "vol_düşük"
        };
        let trend = if !ev.is_nan() && cv > ev {
            "trend_yukarı"
        } else {
            "trend_aşağı"
        };
        *out.entry(format!("{vol}/{trend}")).or_default() += tr.net.as_usd_f64();
    }
    out
}

/// Holdout öncesi kapılar (1–9). Saf hesap: sicile yazmaz → paralel çalıştırılabilir.
pub struct PreHoldout {
    pub report: GauntletReport,
    pub trades: Vec<TradeRecord>,
    pub regime_pnl: BTreeMap<String, f64>,
}

pub fn run_pre_holdout(
    ctx: &Context<'_>,
    g: &Genome,
    trial_number: u64,
    trial_sharpes: &[f64],
) -> Result<PreHoldout, GauntletError> {
    let cfg = ctx.cfg;
    let m = ctx.market;
    let trades = ctx.eval.run(g, m, ctx.cost, None)?;
    let x = nets(&trades);
    let mut rep = GauntletReport {
        genome_name: g.name.clone(),
        genome_hash: g.hash(),
        instrument: m.instrument.symbol.clone(),
        data_version: m.data_version.clone(),
        fill_model: "bar_fill@v1".into(),
        trial_number,
        trades_is: trades.len(),
        expectancy_usd: stats::mean(&x),
        sharpe: stats::sharpe(&x),
        net_usd: x.iter().sum(),
        gates: Vec::new(),
        passed: false,
        failed_gate: None,
    };
    let mut regime_pnl = BTreeMap::new();
    let done = |rep: &mut GauntletReport, r: GateResult| -> bool {
        let pass = r.pass;
        if !pass {
            rep.failed_gate = Some(r.gate);
        }
        rep.gates.push(r);
        !pass
    };

    // 1. Asgari örneklem
    let splits = cpcv_splits(&trades, m.span(), cfg);
    let min_split = splits.iter().map(|s| s.1.len()).min().unwrap_or(0);
    if done(
        &mut rep,
        gate(
            1,
            "asgari örneklem",
            trades.len() >= cfg.min_trades_is && min_split >= cfg.min_trades_cpcv_test,
            serde_json::json!({"trades": trades.len(), "min": cfg.min_trades_is, "min_cpcv_test_trades": min_split, "min_cpcv": cfg.min_trades_cpcv_test}),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }

    // 2. CPCV
    let mut exps: Vec<f64> = splits.iter().map(|s| stats::mean(&s.1)).collect();
    let pos = exps.iter().filter(|e| **e > 0.0).count() as f64 / exps.len().max(1) as f64;
    exps.sort_by(f64::total_cmp);
    let med = exps.get(exps.len() / 2).copied().unwrap_or(0.0);
    if done(
        &mut rep,
        gate(
            2,
            "purged+embargoed CPCV",
            med > 0.0 && pos >= cfg.cpcv_min_positive,
            serde_json::json!({"splits": exps.len(), "median_expectancy": med, "positive_share": pos}),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }

    // 3. Walk-forward
    let (a, b) = (m.span().0.0, m.span().1.0);
    let w = cfg.wf_windows as u64;
    let wl = (b - a) / w;
    let windows: Vec<(usize, f64)> = (0..w)
        .map(|i| {
            let lo = a + i * wl;
            let hi = if i + 1 == w { b + 1 } else { a + (i + 1) * wl };
            let xs: Vec<f64> = trades
                .iter()
                .filter(|t| t.entry_ts.0 >= lo && t.entry_ts.0 < hi)
                .map(|t| t.net.as_usd_f64())
                .collect();
            (xs.len(), stats::mean(&xs))
        })
        .collect();
    if done(
        &mut rep,
        gate(
            3,
            "walk-forward",
            windows.iter().all(|(n, e)| *n > 0 && *e > 0.0),
            serde_json::json!({"windows": windows}),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }

    // 4. Deflated Sharpe
    let mut all_sr = trial_sharpes.to_vec();
    all_sr.push(rep.sharpe);
    let dsr = stats::deflated_sharpe(&x, &all_sr, trial_number);
    if done(
        &mut rep,
        gate(
            4,
            "deflated Sharpe",
            dsr.probability >= cfg.dsr_min,
            serde_json::to_value(&dsr).unwrap_or_default(),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }

    // 5–6. Parametre komşuluğu: PBO + stabilite
    let neigh = atlas_genome::evolve::neighbors(g, cfg.stability_frac);
    let mut neigh_trades: Vec<(String, Vec<TradeRecord>)> = Vec::with_capacity(neigh.len());
    for (p, h) in &neigh {
        neigh_trades.push((p.clone(), ctx.eval.run(h, m, ctx.cost, None)?));
    }
    let blocks = cfg.pbo_blocks as u64;
    let bl = (b - a) / blocks;
    let block_of = |t: &TradeRecord| (((t.entry_ts.0 - a) / bl.max(1)).min(blocks - 1)) as usize;
    let mut perf = vec![vec![0.0; neigh_trades.len() + 1]; cfg.pbo_blocks];
    for t in &trades {
        perf[block_of(t)][0] += t.net.as_usd_f64();
    }
    for (c, (_, ts)) in neigh_trades.iter().enumerate() {
        for t in ts {
            perf[block_of(t)][c + 1] += t.net.as_usd_f64();
        }
    }
    let pbo = stats::pbo_cscv(&perf);
    if done(
        &mut rep,
        gate(
            5,
            "PBO (CSCV)",
            pbo.probability <= cfg.pbo_max,
            serde_json::to_value(&pbo).unwrap_or_default(),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }
    let stab: Vec<(String, usize, f64)> = neigh_trades
        .iter()
        .map(|(p, t)| (p.clone(), t.len(), expectancy(t)))
        .collect();
    let worst = stab.iter().map(|s| s.2).fold(f64::INFINITY, f64::min);
    if done(
        &mut rep,
        gate(
            6,
            "parametre stabilitesi",
            stab.iter().all(|s| s.2 > 0.0),
            serde_json::json!({"neighbors": stab.len(), "worst_expectancy": worst, "detail": stab}),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }

    // 7. Maliyet stresi
    let k = (cfg.cost_stress_mult * 1000.0).round() as i64;
    let stressed = ctx.eval.run(
        g,
        m,
        Cost {
            slippage_mult_milli: ctx.cost.slippage_mult_milli * k / 1000,
            fee_mult_milli: ctx.cost.fee_mult_milli * k / 1000,
        },
        None,
    )?;
    let se = expectancy(&stressed);
    if done(
        &mut rep,
        gate(
            7,
            "maliyet stresi",
            se >= 0.0,
            serde_json::json!({"mult": cfg.cost_stress_mult, "trades": stressed.len(), "expectancy": se}),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }

    // 8. Rejim kırılımı
    regime_pnl = regimes(ctx, &trades);
    let total: f64 = regime_pnl.values().sum();
    let top = regime_pnl.values().copied().fold(f64::MIN, f64::max);
    let share = if total > 0.0 {
        top / total
    } else {
        f64::INFINITY
    };
    if done(
        &mut rep,
        gate(
            8,
            "rejim kırılımı",
            total > 0.0 && share <= cfg.regime_max_share,
            serde_json::json!({"pnl_by_regime": regime_pnl, "top_share": share}),
        ),
    ) {
        return Ok(PreHoldout {
            report: rep,
            trades,
            regime_pnl,
        });
    }

    // 9. Prop Monte Carlo
    let first = a / NANOS_PER_DAY;
    let last = b / NANOS_PER_DAY;
    let days = prop_mc::day_blocks(&trades, first, last);
    let mc = prop_mc::simulate(&days, ctx.prop, cfg.mc_paths, cfg.mc_horizon_days, ctx.seed);
    done(
        &mut rep,
        gate(
            9,
            "prop Monte Carlo",
            mc.pass_probability >= cfg.prop_pass_min && mc.bust_probability <= cfg.prop_bust_max,
            serde_json::json!({"mc": mc, "firm": ctx.prop.firm, "pass_min": cfg.prop_pass_min, "bust_max": cfg.prop_bust_max}),
        ),
    );
    Ok(PreHoldout {
        report: rep,
        trades,
        regime_pnl,
    })
}

/// Kapı 10 (holdout) + sicil kaydı + (düştüyse) ders. Sıralı çalışmalı (bütçe, sayaç).
#[allow(clippy::too_many_arguments)]
pub fn finish(
    ctx: &Context<'_>,
    g: &Genome,
    mut pre: PreHoldout,
    reg: &mut Registry,
    source: &str,
    parent_id: Option<i64>,
    now: UnixNanos,
    cpu_ms: u64,
) -> Result<(GauntletReport, i64), GauntletError> {
    if pre.report.failed_gate.is_none() {
        let r = match ctx.vault.access(reg, &pre.report.genome_hash, now) {
            Err(e) => gate(
                10,
                "holdout",
                false,
                serde_json::json!({"error": e.to_string()}),
            ),
            Ok(acc) => {
                let mut bars: Vec<atlas_core::Bar> = ctx.market.bars.as_ref().clone();
                bars.extend_from_slice(acc.bars);
                let hm = Market {
                    dataset: ctx.market.dataset.clone(),
                    data_version: format!("{}+holdout", ctx.market.data_version),
                    instrument: ctx.market.instrument.clone(),
                    session: ctx.market.session.clone(),
                    bars: std::sync::Arc::new(bars),
                };
                let t = ctx.eval.run(g, &hm, ctx.cost, Some(acc.from))?;
                let e = expectancy(&t);
                gate(
                    10,
                    "holdout",
                    t.len() >= ctx.cfg.holdout_min_trades && e > 0.0,
                    serde_json::json!({"trades": t.len(), "expectancy": e, "access": acc.used, "budget": acc.budget}),
                )
            }
        };
        if !r.pass {
            pre.report.failed_gate = Some(10);
        }
        pre.report.gates.push(r);
        pre.report.passed = pre.report.failed_gate.is_none();
    }
    let rep = pre.report;
    let rec = ExperimentRecord {
        parent_id,
        source: source.to_string(),
        name: rep.genome_name.clone(),
        genome_hash: rep.genome_hash.clone(),
        genome_json: serde_json::to_string(g).unwrap_or_default(),
        tags: g.tags.clone(),
        data_version: rep.data_version.clone(),
        fill_model: rep.fill_model.clone(),
        seed: ctx.seed,
        passed: rep.passed,
        failed_gate: rep.failed_gate,
        trades: rep.trades_is as u64,
        sharpe: (rep.trades_is >= 2).then_some(rep.sharpe),
        report_json: serde_json::to_string(&rep).unwrap_or_default(),
        report_md: rep.to_markdown(),
        cpu_ms,
        llm_tokens: 0,
        created_ts: now.0,
    };
    let id = reg.record(&rec)?;
    if let Some(fg) = rep.failed_gate {
        let why = rep
            .gates
            .iter()
            .find(|x| x.gate == fg)
            .map(|x| format!("kapı {} ({}): {}", x.gate, x.name, x.metrics))
            .unwrap_or_default();
        let what = format!(
            "{} — {} işlem, beklenti ${:.2}",
            g.tokens().into_iter().collect::<Vec<_>>().join(" "),
            rep.trades_is,
            rep.expectancy_usd
        );
        reg.add_lesson(
            id,
            &rep.genome_hash,
            &g.tokens(),
            &g.tags,
            &what,
            &why,
            &serde_json::to_string(&pre.regime_pnl).unwrap_or_default(),
            now.0,
        )?;
    }
    Ok((rep, id))
}

/// Tek aday: 1–10 + kayıt.
pub fn run(
    ctx: &Context<'_>,
    g: &Genome,
    reg: &mut Registry,
    source: &str,
    parent_id: Option<i64>,
    now: UnixNanos,
) -> Result<(GauntletReport, i64), GauntletError> {
    let t0 = std::time::Instant::now();
    let n = reg.trial_count()? + 1;
    let sharpes = reg.sharpes()?;
    let pre = run_pre_holdout(ctx, g, n, &sharpes)?;
    let ms = t0.elapsed().as_millis() as u64;
    finish(ctx, g, pre, reg, source, parent_id, now, ms)
}
