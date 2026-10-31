//! Sahte strateji testi (§4.6, M2 kabul): geleceğe bakmayan ama rastgele parametreli/yapılı N
//! strateji gauntlet'ten geçirilir; geçen oran ≤ %1 olmalı. Arama ne kadar büyürse sahte keşif o kadar
//! artar — gauntlet bunu kontrol edebiliyor mu, bu test ölçer.
//!
//! Deneme sayısı dürüsttür: DSR'deki N = önceki denemeler + bu testteki N aday.

use crate::{Context, GauntletError, finish, run_pre_holdout};
use atlas_core::UnixNanos;
use atlas_genome::Genome;
use atlas_genome::evolve::{random_genome, rng};
use atlas_registry::Registry;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
pub struct FakeTestOutcome {
    pub candidates: usize,
    pub passed_final: usize,
    pub passed_through_gate9: usize,
    pub pass_rate: f64,
    pub max_pass_rate: f64,
    /// Düşülen kapı → aday sayısı ("geçti" dahil).
    pub failed_gate_histogram: BTreeMap<String, usize>,
    pub seconds: f64,
    pub green: bool,
}

pub const MAX_PASS_RATE: f64 = 0.01;

pub fn candidates(n: usize, seed: u64, instrument: &str, risk_usd: u32) -> Vec<Genome> {
    let mut r = rng(seed);
    (0..n)
        .map(|i| {
            random_genome(
                &mut r,
                instrument,
                &format!("sahte-{seed}-{i:04}"),
                risk_usd,
            )
        })
        .collect()
}

/// Adayları paralel değerlendirir (kapı 1–9), sonra sırayla holdout + sicil.
pub fn run(
    ctx: &Context<'_>,
    genomes: &[Genome],
    reg: &mut Registry,
    now: UnixNanos,
) -> Result<FakeTestOutcome, GauntletError> {
    let t0 = std::time::Instant::now();
    let prior = reg.trial_count()?;
    let n_trials = prior + genomes.len() as u64;
    // 1. geçiş: temel koşu → Sharpe dağılımı (V[SR] için), paralel.
    let base: Vec<Result<f64, GauntletError>> = genomes
        .par_iter()
        .map(|g| {
            let t = ctx.eval.run(g, ctx.market, ctx.cost, None)?;
            Ok(crate::stats::sharpe(&crate::eval::nets(&t)))
        })
        .collect();
    let mut sharpes = reg.sharpes()?;
    sharpes.extend(base.iter().flatten().copied());
    // 2. geçiş: kapı 1–9, paralel; sonuçlar indeks sırasında.
    let pres: Vec<Result<crate::PreHoldout, GauntletError>> = genomes
        .par_iter()
        .map(|g| run_pre_holdout(ctx, g, n_trials, &sharpes))
        .collect();
    // 3. holdout + sicil, sıralı (deterministik bütçe ve sayaç).
    let mut hist: BTreeMap<String, usize> = BTreeMap::new();
    let (mut final_pass, mut pre_pass) = (0usize, 0usize);
    for (g, pre) in genomes.iter().zip(pres) {
        let pre = pre?;
        if pre.report.failed_gate.is_none() {
            pre_pass += 1;
        }
        let (rep, _) = finish(ctx, g, pre, reg, "sahte-test", None, now, 0)?;
        let key = rep
            .failed_gate
            .map_or_else(|| "geçti".to_string(), |x| format!("{x:02}"));
        *hist.entry(key).or_default() += 1;
        if rep.passed {
            final_pass += 1;
        }
    }
    let rate = final_pass as f64 / genomes.len().max(1) as f64;
    Ok(FakeTestOutcome {
        candidates: genomes.len(),
        passed_final: final_pass,
        passed_through_gate9: pre_pass,
        pass_rate: rate,
        max_pass_rate: MAX_PASS_RATE,
        failed_gate_histogram: hist,
        seconds: t0.elapsed().as_secs_f64(),
        green: rate <= MAX_PASS_RATE,
    })
}
