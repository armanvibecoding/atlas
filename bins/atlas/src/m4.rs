//! `atlas verify m4` — M4 kabul (§7): (1) paper dolumlarıyla simülasyon arasındaki kayma farkı ölçülür
//! ve kalibrasyon sonrası küçülür; (2) plugin sandbox'ı ağ/dosya/saat erişimini reddeder;
//! (3) dashboard tüm CLI verisini gösterir. Ek: meta-model sinyali gürültüden ayırır; kuyruk kuralı.

use crate::home::Home;
use crate::{Out, now, open_journal};
use anyhow::Result;
use atlas_core::{Event, EventKind, Price, Side, UnixNanos};
use atlas_sim::validation::CheckOutcome;
use clap::CommandFactory;
use rand::{Rng, SeedableRng};
use serde_json::json;

fn check(name: &str, pass: bool, detail: serde_json::Value) -> CheckOutcome {
    CheckOutcome {
        name: name.into(),
        pass,
        detail,
    }
}

/// (1) Kalibrasyon: "canlı" dolumlar bilinen kaymayla (3 tick ± gürültü) üretilir, başlangıç modeli 1 tick.
fn calibration(seed: u64) -> CheckOutcome {
    use atlas_sim::calibration::{
        FillObservation, FillParams, ObservedKind, calibrate, measure, resimulate,
    };
    let mut r = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
    let base = FillParams::default();
    let obs: Vec<FillObservation> = (0..400)
        .map(|i| {
            let side = if i % 2 == 0 { Side::Buy } else { Side::Sell };
            let reference = Price(100_000 + r.random_range(-50..50));
            let live_slip = 3 + r.random_range(-1..=1);
            let sg = i64::from(side.sign());
            FillObservation {
                instrument: "NQ".into(),
                side,
                kind: if i % 3 == 0 {
                    ObservedKind::Stop
                } else {
                    ObservedKind::Market
                },
                qty: 1,
                reference_price: reference,
                sim_price: Price(reference.0 + sg),
                live_price: Price(reference.0 + sg * live_slip),
                filled: true,
            }
        })
        .collect();
    let before = measure(&obs).price.map(|p| p.mean_abs_sim_live_diff_ticks);
    let cal = calibrate(&base, &obs);
    let re = resimulate(&cal, &obs, 1);
    let after = measure(&re).price.map(|p| p.mean_abs_sim_live_diff_ticks);
    let pass = matches!((before, after), (Some(b), Some(a)) if a < b * 0.6);
    check(
        "calibration_reduces_sim_live_gap",
        pass,
        json!({"before_mean_abs_ticks": before, "after_mean_abs_ticks": after, "model": cal.id(),
               "note": "gerçek paper dolumu yok (M5); gözlemler bilinen kaymayla sentetik"}),
    )
}

/// (2) Sandbox: ağ / dosya / saat / rastgelelik importları reddedilir; importsuz modül yüklenir.
fn sandbox() -> CheckOutcome {
    let forbidden = [
        (
            "dosya",
            r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))) (memory (export "memory") 1))"#,
        ),
        (
            "ağ",
            r#"(module (import "wasi_snapshot_preview1" "sock_open" (func (param i32 i32 i32) (result i32))) (memory (export "memory") 1))"#,
        ),
        (
            "saat",
            r#"(module (import "wasi_snapshot_preview1" "clock_time_get" (func (param i32 i64 i32) (result i32))) (memory (export "memory") 1))"#,
        ),
        (
            "rastgele",
            r#"(module (import "wasi_snapshot_preview1" "random_get" (func (param i32 i32) (result i32))) (memory (export "memory") 1))"#,
        ),
    ];
    let mut results = serde_json::Map::new();
    let mut ok = true;
    for (k, wat) in forbidden {
        let r = atlas_plugin::PluginModule::from_bytes(wat.as_bytes());
        let denied = matches!(r, Err(atlas_plugin::PluginError::ForbiddenImport { .. }));
        ok &= denied;
        results.insert(
            k.into(),
            json!(if denied {
                "reddedildi"
            } else {
                "GEÇTİ (hata)"
            }),
        );
    }
    let clean = r#"(module (memory (export "memory") 1)
        (func (export "alloc") (param i32) (result i32) i32.const 1024)
        (func (export "on_event") (param i32 i32) (result i64) i64.const 0))"#;
    let loads = atlas_plugin::PluginModule::from_bytes(clean.as_bytes()).is_ok();
    results.insert(
        "importsuz modül".into(),
        json!(if loads { "yüklendi" } else { "yüklenemedi" }),
    );
    check(
        "plugin_sandbox_denies_network_file_clock",
        ok && loads,
        serde_json::Value::Object(results),
    )
}

/// (3) Dashboard: CLI'daki her yaprak komut ya panelin okuma listesinde ya da gerekçeli dışlamada.
fn dashboard_coverage() -> CheckOutcome {
    fn leaves(c: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        let subs: Vec<&clap::Command> = c
            .get_subcommands()
            .filter(|s| s.get_name() != "help")
            .collect();
        if subs.is_empty() {
            out.push(prefix.trim().to_string());
        }
        for s in subs {
            leaves(s, &format!("{prefix} {}", s.get_name()), out);
        }
    }
    let mut all = Vec::new();
    leaves(&crate::Cli::command(), "", &mut all);
    let reads: Vec<String> = atlas_dashboard::commands::READ_COMMANDS
        .iter()
        .map(|r| r.argv.join(" "))
        .collect();
    let excluded: Vec<&str> = atlas_dashboard::commands::EXCLUDED
        .iter()
        .map(|e| e.0)
        .collect();
    let covered = |c: &str| {
        reads.iter().any(|r| r == c)
            || excluded
                .iter()
                .any(|e| c == *e || c.starts_with(&format!("{e} ")))
    };
    let missing: Vec<&String> = all.iter().filter(|c| !covered(c)).collect();
    let shown: Vec<&String> = all
        .iter()
        .filter(|c| reads.iter().any(|r| r == *c))
        .collect();
    check(
        "dashboard_shows_all_cli_data",
        missing.is_empty(),
        json!({"cli_leaf_commands": all.len(), "shown_in_dashboard": shown, "missing": missing,
               "excluded_with_reason": excluded.len()}),
    )
}

/// Ek: meta-model gerçek sinyali öğrenir, gürültüye kanmaz (purged k-fold AUC).
fn meta_model(seed: u64) -> CheckOutcome {
    use atlas_ml::{CvConfig, Dataset, LogitConfig, Sample, Transform, cross_validate};
    let mut r = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
    let mk = |signal: bool, r: &mut rand_chacha::ChaCha8Rng| -> Vec<Sample> {
        (0..3000u64)
            .map(|i| {
                let x0 = (0..12).map(|_| r.random::<f64>()).sum::<f64>() - 6.0;
                let x1 = (0..12).map(|_| r.random::<f64>()).sum::<f64>() - 6.0;
                let z = if signal { 1.2 * x0 } else { 0.0 }
                    + ((0..12).map(|_| r.random::<f64>()).sum::<f64>() - 6.0);
                Sample {
                    t0: UnixNanos(i * 60_000_000_000),
                    t1: UnixNanos(i * 60_000_000_000 + 30_000_000_000),
                    x: vec![x0, x1],
                    y: z > 0.0,
                }
            })
            .collect()
    };
    let names = vec!["f0".to_string(), "f1".to_string()];
    let run = |s: Vec<Sample>| -> Option<f64> {
        let ds = Dataset::new(names.clone(), Transform::Raw, s).ok()?;
        cross_validate(&ds, &CvConfig::default(), &LogitConfig::default())
            .ok()?
            .auc
    };
    let sig = run(mk(true, &mut r));
    let noise = run(mk(false, &mut r));
    let pass = matches!((sig, noise), (Some(a), Some(b)) if a > 0.65 && (b - 0.5).abs() < 0.05);
    check(
        "meta_model_signal_vs_noise",
        pass,
        json!({"auc_signal": sig, "auc_noise": noise}),
    )
}

pub fn verify_m4(h: &Home, out: &Out) -> Result<bool> {
    let seed = h.cfg.verify.seed;
    let checks = vec![
        calibration(seed),
        sandbox(),
        dashboard_coverage(),
        meta_model(seed),
    ];
    let passed = checks.iter().filter(|c| c.pass).count();
    let green = passed == checks.len();
    if let Ok(mut j) = open_journal(h) {
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| !c.pass)
            .map(|c| c.name.as_str())
            .collect();
        let _ = j.append(&Event::new(
            now(),
            EventKind::ExperimentDone,
            json!({"verify": "m4", "passed": passed, "total": checks.len(), "failed": failed}),
        ));
    }
    out.emit("verify.m4", green, json!({"milestone": "m4", "green": green, "passed": passed, "total": checks.len(), "checks": checks}), || {
        let mut s = format!("verify m4: {passed}/{} {}\n", checks.len(), if green { "YEŞİL" } else { "KIRMIZI" });
        for c in &checks {
            let d = c.detail.to_string();
            let d = if d.len() > 300 { format!("{}…", &d[..d.char_indices().nth(300).map_or(d.len(), |x| x.0)]) } else { d };
            s += &format!("  {} {:<42} {}\n", if c.pass { "OK  " } else { "FAIL" }, c.name, d);
        }
        s.trim_end().to_string()
    });
    Ok(green)
}
