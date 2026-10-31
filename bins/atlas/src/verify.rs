//! `atlas verify m1` — M1 kabul kriterleri (§7):
//! rastgele strateji ve al-tut testleri geçer; prop kural testleri Arman'ın doldurduğu config'le
//! birebir uyar; aynı seed ile iki koşu bit düzeyinde aynı; `journal replay` tutarlı.
//! Ek olarak: config'ler yüklenir, veri deposu dolu ve gerçek veride doğrulamalar geçer.

use crate::home::Home;
use crate::{Out, now};
use anyhow::Result;
use atlas_core::{Event, EventKind, MarketPayload, UnixNanos};
use atlas_data::{DataSource, StoreSource, TimeRange};
use atlas_journal::Journal;
use atlas_risk::RiskEnv;
use atlas_sim::validation::{self, CheckOutcome};
use serde_json::json;

fn fail(name: &str, msg: impl std::fmt::Display) -> CheckOutcome {
    CheckOutcome {
        name: name.into(),
        pass: false,
        detail: json!({"error": msg.to_string()}),
    }
}

fn ok(name: &str, detail: serde_json::Value) -> CheckOutcome {
    CheckOutcome {
        name: name.into(),
        pass: true,
        detail,
    }
}

/// Sentetik bir koşunun olaylarını journal'a yazar, checkpoint atar, replay eder.
fn journal_roundtrip(seed: u64) -> CheckOutcome {
    let name = "journal_replay_synthetic";
    let res = (|| -> Result<CheckOutcome> {
        let env = validation::validation_env().map_err(anyhow::Error::msg)?;
        let inst = atlas_core::InstrumentId(0);
        let trades = atlas_sim::synthetic::random_walk_trades(
            seed,
            inst,
            UnixNanos::from_secs(1_700_000_000),
            50_000,
            atlas_core::units::NANOS_PER_SEC,
            atlas_core::Price(100_000),
        );
        let bars =
            atlas_sim::synthetic::bars_from_trades(&trades, inst, atlas_core::units::NANOS_PER_MIN);
        let mut strats: Vec<Box<dyn atlas_core::Strategy>> =
            vec![Box::new(validation::RandomEntry::new(
                seed,
                inst,
                0.1,
                8,
                8,
                60 * atlas_core::units::NANOS_PER_MIN,
            ))];
        let r = validation::run_sim(
            &env,
            &atlas_sim::BarFill::default(),
            Default::default(),
            bars,
            &mut strats,
        )?;
        let mut j = Journal::open_in_memory()?;
        for (k, e) in r.events.iter().enumerate() {
            j.append(e)?;
            if k % 97 == 0 {
                j.checkpoint(e.ts)?;
            }
        }
        j.checkpoint(UnixNanos(u64::MAX / 2))?;
        let rep = j.replay()?;
        // Journal'dan türetilen gerçekleşmiş net = simülasyonun hesap neti.
        let realized = j.projection().realized_net_micros;
        let pass = rep.consistent() && rep.checkpoints > 1 && realized == r.net_pnl().0;
        Ok(CheckOutcome {
            name: name.into(),
            pass,
            detail: json!({"events": rep.events, "checkpoints": rep.checkpoints, "mismatches": rep.mismatches.len(),
                "journal_realized_micros": realized, "sim_net_micros": r.net_pnl().0}),
        })
    })();
    res.unwrap_or_else(|e| fail(name, format!("{e:#}")))
}

fn real_data_checks(h: &Home, seed: u64) -> Vec<CheckOutcome> {
    let mut out = Vec::new();
    let store = atlas_data::Store::new(h.store_path());
    let manifest = match store.manifest() {
        Ok(m) => m,
        Err(e) => return vec![fail("data_store", e)],
    };
    let mut datasets: Vec<(String, String, String)> = manifest
        .entries
        .values()
        .map(|e| (e.source.clone(), e.symbol.clone(), e.schema.clone()))
        .collect();
    datasets.sort();
    datasets.dedup();
    if datasets.is_empty() {
        return vec![fail(
            "data_present",
            "depoda veri yok — atlas data pull-binance --symbol BTCUSDT --from 2024-11 --to 2026-10",
        )];
    }
    out.push(ok("data_present", json!({"datasets": datasets})));
    let (table, cal) = match (h.instruments(), h.calendar()) {
        (Ok(t), Ok(c)) => (t, c),
        _ => return vec![fail("data_checks", "config yüklenemedi")],
    };
    for (source, symbol, schema) in datasets {
        let Ok(inst) = table.get(&symbol) else {
            out.push(fail(&format!("data_{symbol}"), "instruments.toml'da yok"));
            continue;
        };
        // Doğrulama hesabı (kurgusal) + gerçek enstrüman/takvim. Kayma ölçülmediği için 0;
        // maliyetsiz sonuç testi kaymadan bağımsızdır.
        let env = match validation::validation_env() {
            Ok(v) => RiskEnv {
                rules: v.rules,
                policy: v.policy,
                instruments: table.clone(),
                calendar: cal.clone(),
            },
            Err(e) => {
                out.push(fail("data_env", e));
                continue;
            }
        };
        let mut src = StoreSource::new(atlas_data::Store::new(h.store_path()))
            .with(inst.id, &source, &symbol, &schema);
        let evs: Vec<_> = match src.events(TimeRange {
            from: UnixNanos::ZERO,
            to: UnixNanos::MAX,
        }) {
            Ok(it) => it.collect(),
            Err(e) => {
                out.push(fail(&format!("data_load_{symbol}"), e));
                continue;
            }
        };
        let version = src.version().map(|v| v.0.clone()).unwrap_or_default();
        let ranges: Vec<i64> = evs
            .iter()
            .filter_map(|e| match &e.payload {
                MarketPayload::Bar(b) => Some(b.high - b.low),
                _ => None,
            })
            .collect();
        let avg_range = if ranges.is_empty() {
            1
        } else {
            ranges.iter().sum::<i64>() / ranges.len() as i64
        };
        let stop = (avg_range * 8).max(10);
        out.push(ok(
            &format!("data_loaded_{symbol}"),
            json!({"bars": evs.len(), "version": version, "avg_bar_range_ticks": avg_range}),
        ));
        let mut c = validation::check_random_on_events(
            &format!("random_entry_real_{symbol}"),
            &with_zero_slip(&env),
            inst.id,
            evs.clone(),
            seed,
            stop,
        );
        c.detail["stop_ticks"] = json!(stop);
        out.push(c);
        let mut bh = validation::check_buy_hold(evs, &env);
        bh.name = format!("buy_hold_exact_real_{symbol}");
        out.push(bh);
    }
    out
}

/// Kayma alanı boş enstrümanlar için gerçek veri testinde kayma = 0 (maliyetsiz test buna duyarsız).
fn with_zero_slip(env: &RiskEnv) -> RiskEnv {
    let items: Vec<_> = env
        .instruments
        .iter()
        .cloned()
        .map(|mut i| {
            if i.costs.slippage_ticks.is_none() {
                i.costs.slippage_ticks = Some(0);
            }
            i
        })
        .collect();
    RiskEnv {
        rules: env.rules.clone(),
        policy: env.policy.clone(),
        instruments: atlas_core::InstrumentTable::from_instruments(items),
        calendar: env.calendar.clone(),
    }
}

pub fn m1(h: &Home, out: &Out) -> Result<bool> {
    let seed = h.cfg.verify.seed;
    let mut checks: Vec<CheckOutcome> = Vec::new();

    // Config
    for (n, r) in [
        (
            "config_instruments",
            h.instruments().map(|t| json!({"count": t.len()})),
        ),
        (
            "config_calendar",
            h.calendar()
                .map(|c| json!({"sessions": c.names().collect::<Vec<_>>()})),
        ),
        ("config_risk_policy", h.policy().map(|_| json!({}))),
    ] {
        checks.push(match r {
            Ok(d) => ok(n, d),
            Err(e) => fail(n, format!("{e:#}")),
        });
    }

    // Simülasyon doğrulamaları (sentetik)
    checks.extend(validation::all_sim_checks(seed));

    // Prop kuralları
    match h.prop()? {
        Ok(rules) => {
            checks.push(ok(
                "prop_config_filled",
                json!({"firm": rules.firm, "account": rules.account_label}),
            ));
            checks.extend(validation::prop_conformance(&rules));
        }
        Err(e) => checks.push(fail(
            "prop_config_filled",
            format!(
                "{} — {e}. Arman firmanın yazılı kurallarından doldurmalı (configs/{}).",
                h.cfg.account.prop, h.cfg.account.prop
            ),
        )),
    }

    // Journal
    checks.push(journal_roundtrip(seed));
    match Journal::open(&h.journal_path()) {
        Ok(j) => match j.replay() {
            Ok(r) => checks.push(CheckOutcome {
                name: "journal_replay_live".into(),
                pass: r.consistent(),
                detail: json!({"events": r.events, "checkpoints": r.checkpoints, "mismatches": r.mismatches}),
            }),
            Err(e) => checks.push(fail("journal_replay_live", e)),
        },
        Err(e) => checks.push(fail("journal_replay_live", e)),
    }

    // Gerçek veri
    checks.extend(real_data_checks(h, seed));

    let passed = checks.iter().filter(|c| c.pass).count();
    let all = checks.len();
    let green = passed == all;

    // Sonucu journal'a yaz (deney/doğrulama kaydı).
    if let Ok(mut j) = Journal::open(&h.journal_path()) {
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| !c.pass)
            .map(|c| c.name.as_str())
            .collect();
        let _ = j.append(&Event::new(
            now(),
            EventKind::ExperimentDone,
            json!({"verify": "m1", "passed": passed, "total": all, "failed": failed}),
        ));
    }

    out.emit("verify.m1", green, json!({"milestone": "m1", "green": green, "passed": passed, "total": all, "checks": checks}), || {
        let mut s = format!("verify m1: {passed}/{all} {}\n", if green { "YEŞİL" } else { "KIRMIZI" });
        for c in &checks {
            let d = c.detail.to_string();
            let d = if d.len() > 220 { format!("{}…", &d[..d.char_indices().nth(220).map_or(d.len(), |x| x.0)]) } else { d };
            s += &format!("  {} {:<40} {}\n", if c.pass { "OK  " } else { "FAIL" }, c.name, d);
        }
        s.trim_end().to_string()
    });
    Ok(green)
}
