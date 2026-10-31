//! M2 komutları: `gauntlet`, `strategies`, `experiments`, `verify m2`.

use crate::home::Home;
use crate::{Out, now, printable};
use anyhow::{Context as _, Result, anyhow, bail};
use atlas_core::{Bar, Event, EventKind, SessionSpec, UnixNanos};
use atlas_features::FeatureStore;
use atlas_gauntlet::eval::{Cost, Evaluator, Market, research_env};
use atlas_gauntlet::holdout::{HoldoutVault, split};
use atlas_gauntlet::{Context, GauntletConfig, fake};
use atlas_genome::Genome;
use atlas_registry::Registry;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub fn registry_path(h: &Home) -> PathBuf {
    h.path(&h.cfg.paths.registry)
}

pub fn open_registry(h: &Home) -> Result<Registry> {
    let p = registry_path(h);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    Registry::open(&p).with_context(|| format!("sicil açılamadı: {}", p.display()))
}

pub fn gauntlet_cfg(h: &Home) -> Result<GauntletConfig> {
    GauntletConfig::from_atlas_toml(&h.atlas_toml_text).map_err(|e| anyhow!("atlas.toml: {e}"))
}

/// Genom dosyası: yol ya da `strategies/<ad>.toml`.
pub fn load_genome(h: &Home, s: &str) -> Result<(Genome, PathBuf)> {
    let p = PathBuf::from(s);
    let path = if p.exists() {
        p
    } else {
        h.root.join("strategies").join(format!("{s}.toml"))
    };
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("{} okunamadı", path.display()))?;
    Ok((
        Genome::from_toml_str(&text).map_err(|e| anyhow!("{}: {e}", path.display()))?,
        path,
    ))
}

pub struct Loaded {
    pub market: Market,
    pub vault: HoldoutVault,
    pub env: atlas_risk::RiskEnv,
}

/// Bir sembolün tüm 1m barlarını yükler, holdout'u ayırır.
pub fn load_market(h: &Home, symbol: &str, cfg: &GauntletConfig) -> Result<Loaded> {
    let table = h.instruments()?;
    let inst = table.get(symbol).map_err(|e| anyhow!("{e}"))?.clone();
    let cal = h.calendar()?;
    let session: SessionSpec = cal
        .session(&inst.session)
        .map_err(|e| anyhow!("{e}"))?
        .clone();
    let store = atlas_data::Store::new(h.store_path());
    let (bars, ver): (Vec<Bar>, _) = store
        .load_bars(
            "binance_futures",
            symbol,
            "ohlcv_1m",
            UnixNanos::ZERO,
            UnixNanos::MAX,
        )
        .map_err(|e| anyhow!("{e}"))?;
    let (is_bars, vault) = split(symbol, bars, cfg.holdout_days, cfg.holdout_budget);
    let env = research_env(&table, &cal)?;
    Ok(Loaded {
        market: Market {
            dataset: symbol.to_string(),
            data_version: format!("{}-is{}", ver.0, cfg.holdout_days),
            instrument: inst,
            session,
            bars: Arc::new(is_bars),
        },
        vault,
        env,
    })
}

pub fn gauntlet_run(h: &Home, out: &Out, genome: &str, source: &str) -> Result<bool> {
    let cfg = gauntlet_cfg(h)?;
    let (g, path) = load_genome(h, genome)?;
    let prop = h.prop()?.map_err(|e| anyhow!("{e}"))?;
    let l = load_market(h, &g.instrument, &cfg)?;
    let store = FeatureStore::new();
    let ev = Evaluator::new(l.env.clone(), &store);
    let ctx = Context {
        eval: &ev,
        market: &l.market,
        vault: &l.vault,
        prop: &prop,
        cfg: &cfg,
        seed: h.cfg.verify.seed,
        cost: atlas_gauntlet::eval::Cost::BASE,
    };
    let mut reg = open_registry(h)?;
    let similar = reg.similar_dead(&g.tokens(), &g.tags, 0.15)?;
    let (rep, id) = atlas_gauntlet::run(&ctx, &g, &mut reg, source, None, now())?;
    let mut j = crate::open_journal(h)?;
    j.append(&Event::new(
        now(),
        if rep.passed { EventKind::GauntletPass } else { EventKind::ExperimentDone },
        json!({"experiment": id, "name": rep.genome_name, "passed": rep.passed, "failed_gate": rep.failed_gate,
               "trades": rep.trades_is, "expectancy_usd": rep.expectancy_usd, "trial": rep.trial_number}),
    ))?;
    let md = rep.to_markdown();
    out.emit(
        "gauntlet.run",
        true,
        json!({"experiment": id, "file": path, "report": rep, "similar_dead_ideas": similar.iter().map(|(l, d)| json!({"lesson": l.id, "distance": d, "why": l.why_died})).collect::<Vec<_>>()}),
        || {
            let mut s = md.clone();
            if !similar.is_empty() {
                s += &format!("\nBilgi tabanı: {} benzer ölü fikir var (en yakın mesafe {:.2}).", similar.len(), similar[0].1);
            }
            s
        },
    );
    Ok(true)
}

/// Yalnız temel değerlendirme + süre (sicile yazmaz; deneme sayılmaz).
pub fn gauntlet_eval(h: &Home, out: &Out, genome: &str, frictionless: bool) -> Result<bool> {
    let cost = if frictionless {
        Cost {
            slippage_mult_milli: 0,
            fee_mult_milli: 0,
        }
    } else {
        Cost::BASE
    };
    let cfg = gauntlet_cfg(h)?;
    let (g, _) = load_genome(h, genome)?;
    let l = load_market(h, &g.instrument, &cfg)?;
    let store = FeatureStore::new();
    let ev = Evaluator::new(l.env.clone(), &store);
    let t0 = std::time::Instant::now();
    let _ = ev.run(&g, &l.market, cost, None)?;
    let cold = t0.elapsed().as_secs_f64() * 1000.0;
    let t1 = std::time::Instant::now();
    let t = ev.run(&g, &l.market, cost, None)?;
    let warm = t1.elapsed().as_secs_f64() * 1000.0;
    let x = atlas_gauntlet::eval::nets(&t);
    let d = json!({"bars": l.market.bars.len(), "trades": t.len(), "expectancy_usd": atlas_gauntlet::stats::mean(&x),
        "net_usd": x.iter().sum::<f64>(), "ms_cold": cold, "ms_warm": warm, "frictionless": frictionless});
    out.emit("gauntlet.eval", true, d.clone(), || {
        format!(
            "{}: {} bar, {} işlem, beklenti ${:.2}, net ${:.0} — süre {:.0} ms (feature hesabı dahil), {:.0} ms (önbellekten)",
            g.name, d["bars"], d["trades"], d["expectancy_usd"].as_f64().unwrap_or(0.0),
            d["net_usd"].as_f64().unwrap_or(0.0), cold, warm
        )
    });
    Ok(true)
}

fn strategy_files(h: &Home) -> Result<Vec<PathBuf>> {
    let dir = h.root.join("strategies");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| dir.display().to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    v.sort();
    Ok(v)
}

pub fn strategies_list(h: &Home, out: &Out) -> Result<bool> {
    let reg = open_registry(h)?;
    let mut rows = Vec::new();
    for p in strategy_files(h)? {
        let text = std::fs::read_to_string(&p)?;
        let row = match Genome::from_toml_str(&text) {
            Ok(g) => {
                let last = reg.experiments_for_hash(&g.hash(), 1)?.into_iter().next();
                json!({"name": g.name, "instrument": g.instrument, "hash": &g.hash()[..12], "tags": g.tags,
                    "stage": if last.as_ref().is_some_and(|e| e.passed) { "gauntlet_geçti" } else { "aday" },
                    "last_experiment": last})
            }
            Err(e) => json!({"file": p, "error": e.to_string()}),
        };
        rows.push(row);
    }
    out.emit("strategies.list", true, json!(rows), || {
        rows.iter()
            .map(|r| {
                let last = &r["last_experiment"];
                let res = if last.is_null() {
                    "henüz gauntlet yok".to_string()
                } else if last["passed"].as_bool() == Some(true) {
                    "GEÇTİ".to_string()
                } else {
                    format!(
                        "kapı {} düştü ({} işlem)",
                        last["failed_gate"], last["trades"]
                    )
                };
                format!(
                    "{:<26} {:<9} {:<16} {}",
                    printable(r["name"].as_str().unwrap_or("?")),
                    r["instrument"].as_str().unwrap_or(""),
                    r["stage"].as_str().unwrap_or(""),
                    res
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(true)
}

pub fn strategies_show(h: &Home, out: &Out, name: &str) -> Result<bool> {
    let (g, path) = load_genome(h, name)?;
    let reg = open_registry(h)?;
    let last = reg.experiments_for_hash(&g.hash(), 1)?.into_iter().next();
    let md = match &last {
        Some(e) => reg.report_md(e.id)?,
        None => None,
    };
    out.emit(
        "strategies.show",
        true,
        json!({"file": path, "genome": g, "hash": g.hash(), "last_experiment": last, "report_md": md}),
        || {
            format!(
                "{}\n{}\n{}",
                path.display(),
                g.to_toml(),
                md.unwrap_or_else(|| "(henüz gauntlet raporu yok)".into())
            )
        },
    );
    Ok(true)
}

pub fn experiments_summary(h: &Home, out: &Out) -> Result<bool> {
    let reg = open_registry(h)?;
    let s = reg.summary()?;
    let recent = reg.experiments(10, None)?;
    out.emit(
        "experiments.summary",
        true,
        json!({"summary": s, "recent": recent}),
        || {
            let mut t = format!(
                "toplam deneme: {} (geri alınamaz) · geçen: {} · ders: {} · holdout erişimi: {}\n",
                s.trials, s.passed, s.lessons, s.holdout_accesses
            );
            t += "kaynak → (deneme, geçen):\n";
            for (k, (n, p)) in &s.by_source {
                t += &format!("  {k}: {n}, {p}\n");
            }
            t += "düşülen kapı dağılımı:\n";
            for (k, n) in &s.failed_gate_histogram {
                t += &format!("  {k}: {n}\n");
            }
            t += "son 10:\n";
            for e in &recent {
                t += &format!(
                    "  #{} {:<24} {:<10} {} işlem={}\n",
                    e.id,
                    printable(&e.name),
                    e.source,
                    if e.passed {
                        "GEÇTİ".to_string()
                    } else {
                        format!("kapı {:?}", e.failed_gate)
                    },
                    e.trades
                );
            }
            t.trim_end().to_string()
        },
    );
    Ok(true)
}

pub const REFERENCE: &[&str] = &[
    "trend_pullback",
    "sweep_mss",
    "vwap_mean_reversion",
    "opening_range_breakout",
    "cvd_divergence",
];

fn check(name: &str, pass: bool, detail: serde_json::Value) -> atlas_sim::validation::CheckOutcome {
    atlas_sim::validation::CheckOutcome {
        name: name.into(),
        pass,
        detail,
    }
}

/// `atlas verify m2`: lookahead dedektörü, sahte strateji testi (≤%1), 5 referans raporu, performans.
pub fn verify_m2(h: &Home, out: &Out, fake_n: usize) -> Result<bool> {
    if fake_n == 0 {
        bail!("--fake-n 0 olamaz");
    }
    let cfg = gauntlet_cfg(h)?;
    let mut checks = Vec::new();
    let symbol = "BTCUSDT";
    let l = load_market(h, symbol, &cfg)?;

    // 1. Lookahead dedektörü: tüm feature'lar gerçek veride zaman noktalı + hileli feature yakalanır.
    let sample: Vec<Bar> = l.market.bars.iter().take(30_000).copied().collect();
    let res = atlas_features::lookahead::check_all(&sample, &l.market.session);
    let bad: Vec<String> = res
        .iter()
        .filter_map(|r| r.as_ref().err().map(|v| format!("{v:?}")))
        .collect();
    checks.push(check(
        "lookahead_all_features_point_in_time",
        bad.is_empty(),
        json!({"checked": res.len(), "violations": bad}),
    ));
    let cut = atlas_features::lookahead::default_cuts(sample.len());
    let caught = atlas_features::lookahead::check_calc(
        "cheater",
        &atlas_features::lookahead::Cheater::factory,
        &sample,
        &l.market.session,
        &cut,
    )
    .is_err();
    checks.push(check(
        "lookahead_detector_catches_cheater",
        caught,
        json!({}),
    ));

    // 2. Performans: tek genom değerlendirmesi (önbellekli feature'larla) < 200 ms (§8).
    let store = FeatureStore::new();
    let ev = Evaluator::new(l.env.clone(), &store);
    let (g, _) = load_genome(h, "trend_pullback")?;
    let _ = ev.run(&g, &l.market, Cost::BASE, None)?;
    let t0 = std::time::Instant::now();
    let _ = ev.run(&g, &l.market, Cost::BASE, None)?;
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    // NQ 2 yıl ≈ 717 bin 1m bar (23 sa × 5 gün × 52 hafta × 2). BTC 7/24 olduğundan daha çok bar
    // içerir; hedef bar sayısına oranlanarak karşılaştırılır, ham süre de raporlanır.
    let nq_equiv = ms * 717_000.0 / l.market.bars.len().max(1) as f64;
    checks.push(check(
        "perf_single_genome_eval_lt_200ms",
        nq_equiv < 200.0,
        json!({"ms_raw": ms, "bars": l.market.bars.len(), "ms_nq_2y_equivalent": nq_equiv,
               "note": "spec: 2 yıl NQ 1m; NQ verisi yok (M0) → BTCUSDT, bar sayısıyla ölçeklendi"}),
    ));

    // 3. Sahte strateji testi (ayrı, bellek içi sicil — gerçek sayacı şişirmez ama kendi N'ini sayar).
    let prop = h.prop()?.map_err(|e| anyhow!("{e}"))?;
    let fake_cfg = GauntletConfig {
        holdout_budget: fake_n as u64,
        mc_paths: 500,
        ..cfg.clone()
    };
    // Gerçek holdout'a dokunulmaz (kural 8): sahte test in-sample'ın son döneminden kendi kasasını ayırır.
    let (fis, fvault) = split(
        symbol,
        l.market.bars.as_ref().clone(),
        cfg.holdout_days,
        fake_n as u64,
    );
    let fm = Market {
        bars: Arc::new(fis),
        data_version: format!("{}-fake", l.market.data_version),
        ..l.market.clone()
    };
    let fctx = Context {
        eval: &ev,
        market: &fm,
        vault: &fvault,
        prop: &prop,
        cfg: &fake_cfg,
        seed: h.cfg.verify.seed,
        cost: atlas_gauntlet::eval::Cost::BASE,
    };
    let mut freg = Registry::open_in_memory()?;
    let genomes = fake::candidates(fake_n, h.cfg.verify.seed, symbol, 1000);
    let fo = fake::run(&fctx, &genomes, &mut freg, now())?;
    checks.push(check(
        "fake_strategy_pass_rate_le_1pct",
        fo.green,
        serde_json::to_value(&fo)?,
    ));
    // Daha sert varyant: maliyet sıfır → gürültünün beklentisi ≈ 0, ücretler adayı otomatik öldüremez;
    // eleme yalnız istatistik kapılarıyla (CPCV, WF, DSR, PBO, stabilite, rejim, MC, holdout) yapılmalı.
    let zctx = Context {
        cost: Cost::ZERO,
        ..fctx
    };
    let mut zreg = Registry::open_in_memory()?;
    let zo = fake::run(&zctx, &genomes, &mut zreg, now())?;
    checks.push(check(
        "fake_strategy_frictionless_pass_rate_le_1pct",
        zo.green,
        serde_json::to_value(&zo)?,
    ));

    // 4. Beş referans stratejinin gauntlet raporu sicilde (güncel veri sürümüyle).
    let reg = open_registry(h)?;
    for name in REFERENCE {
        // Güncel genom dosyasının hash'iyle eşleşen rapor aranır (dosya değiştiyse eski rapor sayılmaz).
        let hash = load_genome(h, name)?.0.hash();
        let e = reg.experiments_for_hash(&hash, 1)?.into_iter().next();
        let ok = e.is_some();
        checks.push(check(
            &format!("reference_report_{name}"),
            ok,
            match e {
                Some(e) => json!({"experiment": e.id, "passed": e.passed, "failed_gate": e.failed_gate, "trades": e.trades}),
                None => json!({"error": format!("rapor yok — atlas gauntlet run {name}")}),
            },
        ));
    }

    let passed = checks.iter().filter(|c| c.pass).count();
    let green = passed == checks.len();
    if let Ok(mut j) = crate::open_journal(h) {
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| !c.pass)
            .map(|c| c.name.as_str())
            .collect();
        let _ = j.append(&Event::new(
            now(),
            EventKind::ExperimentDone,
            json!({"verify": "m2", "passed": passed, "total": checks.len(), "failed": failed}),
        ));
    }
    out.emit(
        "verify.m2",
        green,
        json!({"milestone": "m2", "green": green, "passed": passed, "total": checks.len(), "checks": checks}),
        || {
            let mut s = format!("verify m2: {passed}/{} {}\n", checks.len(), if green { "YEŞİL" } else { "KIRMIZI" });
            for c in &checks {
                let d = c.detail.to_string();
                let d = if d.len() > 240 { format!("{}…", &d[..d.char_indices().nth(240).map_or(d.len(), |x| x.0)]) } else { d };
                s += &format!("  {} {:<40} {}\n", if c.pass { "OK  " } else { "FAIL" }, c.name, d);
            }
            s.trim_end().to_string()
        },
    );
    Ok(green)
}
