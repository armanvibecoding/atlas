//! M3 komutları: `daemon`, `report`, `strategy pause|demote|resume`, `budget`, `approvals`,
//! `approve` (yalnız Arman, anahtar), `verify m3`; daemon'un ajan döngüsü.

use crate::daemon::{self, AgentsCfg, DaemonOpts, M3Toml, m3_toml};
use crate::home::Home;
use crate::m2::open_registry;
use crate::{Out, now, open_journal, printable};
use anyhow::{Context as _, Result, anyhow, bail};
use atlas_core::calendar::format_utc;
use atlas_core::units::NANOS_PER_DAY;
use atlas_core::{Event, EventKind, Money, Priority, UnixNanos};
use atlas_journal::EventFilter;
use atlas_registry::Registry;
use atlas_registry::ladder::Stage;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn actor() -> String {
    if std::env::var("ATLAS_ROLE").is_ok_and(|r| r.eq_ignore_ascii_case("grok")) {
        "grok".into()
    } else {
        "arman".into()
    }
}

fn pricing(a: &AgentsCfg) -> Option<atlas_agents::Pricing> {
    Some(atlas_agents::Pricing {
        input_per_mtok: Money::parse_usd(&a.input_usd_per_mtok).ok()?,
        output_per_mtok: Money::parse_usd(&a.output_usd_per_mtok).ok()?,
    })
}

/// Bugün (UTC) journal'a yazılmış ajan harcaması: (USD, token).
fn spent_today(h: &Home, t: UnixNanos) -> (Money, u64) {
    let day0 = UnixNanos(t.0 - t.0 % NANOS_PER_DAY);
    let Ok(j) = open_journal(h) else {
        return (Money::ZERO, 0);
    };
    let evs = j
        .read(&EventFilter {
            since_ts: Some(day0),
            kind: Some(EventKind::AgentCall),
            ..Default::default()
        })
        .unwrap_or_default();
    let mut usd = Money::ZERO;
    let mut tok = 0u64;
    for e in evs {
        usd += Money(e.event.payload["usd"].as_i64().unwrap_or(0));
        tok += e.event.payload["input_tokens"].as_u64().unwrap_or(0)
            + e.event.payload["output_tokens"].as_u64().unwrap_or(0);
    }
    (usd, tok)
}

/// Daemon'un ajan döngüsü: sağlayıcı + fiyat + bütçe yoksa hiçbir çağrı yapılmaz.
pub fn agent_cycle(
    h: &Home,
    cfg: &M3Toml,
    reg: &mut Registry,
    t: UnixNanos,
    symbol: &str,
) -> Vec<Event> {
    let a = &cfg.agents;
    let daily = Money::parse_usd(&a.daily_usd).unwrap_or(Money::ZERO);
    let Some(p) = pricing(a) else {
        return vec![
            Event::new(
                t,
                EventKind::ConfigLoaded,
                json!({"agents": "devre dışı: fiyat (input/output_usd_per_mtok) doldurulmamış"}),
            )
            .with_key("agents_off"),
        ];
    };
    if daily.0 <= 0 {
        return vec![
            Event::new(
                t,
                EventKind::ConfigLoaded,
                json!({"agents": "devre dışı: günlük bütçe 0"}),
            )
            .with_key("agents_off"),
        ];
    }
    let provider: Box<dyn atlas_agents::provider::LlmProvider> = match a.provider.as_str() {
        "anthropic" => match atlas_agents::provider::Anthropic::from_env(&a.model) {
            Ok(x) => Box::new(x),
            Err(e) => {
                return vec![
                    Event::new(t, EventKind::ConfigLoaded, json!({"agents": e}))
                        .with_key("agents_off"),
                ];
            }
        },
        "xai" => match atlas_agents::provider::Xai::from_env(&a.model) {
            Ok(x) => Box::new(x),
            Err(e) => {
                return vec![
                    Event::new(t, EventKind::ConfigLoaded, json!({"agents": e}))
                        .with_key("agents_off"),
                ];
            }
        },
        other => {
            return vec![
                Event::new(
                    t,
                    EventKind::ConfigLoaded,
                    json!({"agents": format!("bilinmeyen sağlayıcı {other}")}),
                )
                .with_key("agents_off"),
            ];
        }
    };
    let mut bt = atlas_risk::BudgetTracker::new(atlas_risk::LlmBudget {
        daily_usd: daily,
        daily_tokens: a.daily_tokens,
    });
    let day = atlas_core::calendar::to_utc(t).date_naive();
    let (usd, tok) = spent_today(h, t);
    bt.charge(day, t, usd, tok);
    let summary = reg.summary().ok();
    let lessons: Vec<String> = reg
        .lessons()
        .unwrap_or_default()
        .into_iter()
        .rev()
        .take(15)
        .map(|l| {
            format!(
                "- {} → {}",
                l.what.chars().take(160).collect::<String>(),
                l.why_died.chars().take(120).collect::<String>()
            )
        })
        .collect();
    let context = format!(
        "Enstrüman: {symbol} 1m. Sicil: {}. Son ölü fikirler:\n{}\nÜcret bulgusu: 1m BTC'de dar stop + taker ücreti yapısal kayıp; geniş stop / uzun vade öncelikli.",
        serde_json::to_string(&summary).unwrap_or_default(),
        lessons.join("\n")
    );
    let mut b = atlas_agents::Budgeted {
        provider: provider.as_ref(),
        pricing: p,
        budget: &mut bt,
        day,
        now: t,
        events: Vec::new(),
    };
    let r = atlas_agents::research_cycle(
        &mut b,
        reg,
        &context,
        symbol,
        cfg.search.risk_usd,
        a.hypotheses_per_cycle,
    );
    let mut ev = std::mem::take(&mut b.events);
    ev.push(Event::new(t, EventKind::SearchCycle, json!({"agents": match r { Ok(x) => serde_json::to_value(x).unwrap_or_default(), Err(e) => json!({"error": e.to_string()}) }})));
    ev
}

pub fn daemon_run(
    h: &Home,
    out: &Out,
    replay_days: Option<u64>,
    max_ticks: Option<u64>,
) -> Result<bool> {
    let opts = DaemonOpts::from_home(h, replay_days, max_ticks);
    let s = daemon::run(h, &opts)?;
    out.emit("daemon.run", true, serde_json::to_value(&s)?, || {
        format!("daemon durdu ({}): {} tick, {} bar, {} olay, {} tarama, {} gauntlet ({} geçti), outbox {:?}",
            s.stopped_by, s.ticks, s.bars, s.events, s.screened, s.gauntlet_runs, s.gauntlet_passed, s.outbox)
    });
    Ok(true)
}

pub fn report(h: &Home, out: &Out, kind: &str) -> Result<bool> {
    let span = match kind {
        "daily" => NANOS_PER_DAY,
        "weekly" => 7 * NANOS_PER_DAY,
        k => bail!("report daily|weekly, {k:?} değil"),
    };
    let t = now();
    let since = t.minus(span);
    let j = open_journal(h)?;
    let evs = j.read(&EventFilter {
        since_ts: Some(since),
        ..Default::default()
    })?;
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    let mut high = Vec::new();
    for e in &evs {
        *by_kind.entry(e.event.kind.as_str().into()).or_default() += 1;
        if e.event.priority == Priority::High && high.len() < 20 {
            high.push(json!({"time": format_utc(e.event.ts), "kind": e.event.kind, "payload": e.event.payload}));
        }
    }
    let reg = open_registry(h)?;
    let exps = reg.experiments(100_000, None)?;
    let new_trials = exps.iter().filter(|e| e.created_ts >= since.0).count();
    let passed = exps
        .iter()
        .filter(|e| e.created_ts >= since.0 && e.passed)
        .count();
    let stages = reg.all_current_stages()?;
    let pending = reg.pending_approvals()?;
    let kills = j.projection().kills.clone();
    let s = reg.summary()?;
    let d = json!({"kind": kind, "since": format_utc(since), "events": evs.len(), "by_kind": by_kind, "high": high,
        "new_trials": new_trials, "new_passed": passed, "total_trials": s.trials, "stages": stages,
        "pending_approvals": pending, "kills": kills});
    out.emit(&format!("report.{kind}"), true, d.clone(), || {
        let mut r = format!(
            "Atlas {} raporu — {} → {}\n",
            if kind == "daily" {
                "günlük"
            } else {
                "haftalık"
            },
            format_utc(since),
            format_utc(t)
        );
        r += &format!(
            "• {} olay; HIGH: {}\n",
            evs.len(),
            d["high"].as_array().map_or(0, Vec::len)
        );
        r += &format!(
            "• yeni deneme: {new_trials} (geçen {passed}); toplam deneme {}\n",
            s.trials
        );
        let live: Vec<String> = stages
            .iter()
            .filter(|x| x.stage.is_live())
            .map(|x| format!("{} ({})", printable(&x.name), x.stage))
            .collect();
        r += &format!(
            "• canlıdaki stratejiler: {}\n",
            if live.is_empty() {
                "yok".into()
            } else {
                live.join(", ")
            }
        );
        r += &format!("• bekleyen onay: {}\n", pending.len());
        r += &format!(
            "• kill switch: {}\n",
            if kills.is_empty() {
                "yok".into()
            } else {
                kills.keys().cloned().collect::<Vec<_>>().join(", ")
            }
        );
        for (k, n) in &by_kind {
            r += &format!("    {k}: {n}\n");
        }
        r.trim_end().to_string()
    });
    Ok(true)
}

/// Strateji kimliği: ad ya da hash öneki.
fn resolve(reg: &Registry, id: &str) -> Result<(String, String)> {
    let all = reg.all_current_stages()?;
    let m: Vec<_> = all
        .iter()
        .filter(|s| s.name == id || (id.len() >= 6 && s.genome_hash.starts_with(id)))
        .collect();
    match m.as_slice() {
        [one] => Ok((one.genome_hash.clone(), one.name.clone())),
        [] => bail!("merdivende böyle strateji yok: {id}"),
        _ => bail!("{id} birden çok stratejiyle eşleşti; hash öneki kullan"),
    }
}

pub fn strategy(h: &Home, out: &Out, action: &str, id: &str, reason: &str) -> Result<bool> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().any(char::is_control) || reason.chars().count() > 500 {
        bail!("gerekçe zorunlu (≤500 karakter, kontrol karakteri yok)");
    }
    let mut reg = open_registry(h)?;
    let (hash, name) = resolve(&reg, id)?;
    let who = actor();
    let t = now();
    let (kind, to) = match action {
        "pause" => {
            reg.pause(&hash, reason, &who, t.0)
                .map_err(|e| anyhow!("{e}"))?;
            (EventKind::StageChange, Stage::Paused)
        }
        "resume" => (
            EventKind::StageChange,
            reg.resume(&hash, reason, &who, t.0)
                .map_err(|e| anyhow!("{e}"))?,
        ),
        "demote" => (
            EventKind::Demotion,
            reg.demote(&hash, reason, &who, t.0)
                .map_err(|e| anyhow!("{e}"))?,
        ),
        a => bail!("strategy pause|demote|resume, {a:?} değil"),
    };
    let mut j = open_journal(h)?;
    let seq = j.append(&Event::new(
        t,
        kind,
        json!({"strategy": name, "action": action, "to": to, "reason": reason, "by": who}),
    ))?;
    out.emit(
        &format!("strategy.{action}"),
        true,
        json!({"strategy": name, "hash": hash, "stage": to, "seq": seq}),
        || format!("{} → {} (#{seq})", printable(&name), to),
    );
    Ok(true)
}

pub fn budget(h: &Home, out: &Out) -> Result<bool> {
    let cfg = m3_toml(h)?;
    let (usd, tok) = spent_today(h, now());
    let daily = Money::parse_usd(&cfg.agents.daily_usd).unwrap_or(Money::ZERO);
    let reg = open_registry(h)?;
    let s = reg.summary()?;
    let d = json!({"llm": {"provider": cfg.agents.provider, "model": cfg.agents.model, "daily_usd": daily.as_usd_f64(),
            "daily_tokens": cfg.agents.daily_tokens, "spent_today_usd": usd.as_usd_f64(), "spent_today_tokens": tok,
            "pricing_set": pricing(&cfg.agents).is_some(), "enabled": cfg.agents.provider != "none" && daily.0 > 0 && pricing(&cfg.agents).is_some()},
        "data": {"databento_cap_usd": h.data_cap()?.as_usd_f64()},
        "search": {"evals_per_cycle": cfg.search.evals_per_cycle, "trials_total": s.trials}});
    out.emit("budget", true, d.clone(), || {
        format!("LLM: {} — bugün ${:.2} / ${:.2}, {} token{}\nveri: Databento tavanı ${:.2}\narama: döngü başına {} değerlendirme, toplam deneme {}",
            cfg.agents.provider, usd.as_usd_f64(), daily.as_usd_f64(), tok,
            if d["llm"]["enabled"].as_bool() == Some(true) { "" } else { " (ajanlar KAPALI)" },
            d["data"]["databento_cap_usd"].as_f64().unwrap_or(0.0), cfg.search.evals_per_cycle, s.trials)
    });
    Ok(true)
}

pub fn approvals(h: &Home, out: &Out, show: Option<i64>) -> Result<bool> {
    let reg = open_registry(h)?;
    let all = reg.approvals()?;
    match show {
        Some(id) => {
            let a = all
                .into_iter()
                .find(|a| a.id == id)
                .ok_or_else(|| anyhow!("onay #{id} yok"))?;
            out.emit("approvals.show", true, serde_json::to_value(&a)?, || {
                serde_json::to_string_pretty(&a).unwrap_or_default()
            });
        }
        None => {
            let pending: Vec<_> = all
                .iter()
                .filter(|a| a.decision.is_none())
                .cloned()
                .collect();
            out.emit(
                "approvals.list",
                true,
                json!({"pending": pending, "total": all.len()}),
                || {
                    if pending.is_empty() {
                        "bekleyen onay yok".into()
                    } else {
                        pending
                            .iter()
                            .map(|a| {
                                format!(
                                    "#{} {} {} — {}",
                                    a.id,
                                    a.kind,
                                    &a.subject[..a.subject.len().min(12)],
                                    a.payload
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                },
            );
        }
    }
    Ok(true)
}

pub fn approve(h: &Home, out: &Out, id: i64, reject: bool) -> Result<bool> {
    let cfg = m3_toml(h)?;
    let want = cfg.approvals.key_sha256.trim().to_ascii_lowercase();
    if want.is_empty() {
        bail!("onay kapalı: atlas.toml [approvals] key_sha256 boş (Arman anahtarını tanımlamalı)");
    }
    let key = std::env::var("ATLAS_APPROVER_KEY").context("ATLAS_APPROVER_KEY yok")?;
    let got: String = Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != want {
        bail!("onay anahtarı geçersiz");
    }
    let mut reg = open_registry(h)?;
    let t = now();
    let a = reg
        .decide(id, !reject, "arman", t.0)
        .map_err(|e| anyhow!("{e}"))?;
    let mut j = open_journal(h)?;
    j.append(&Event::new(
        t,
        EventKind::StageChange,
        json!({"approval": id, "kind": a.kind, "decision": a.decision}),
    ))?;
    out.emit("approve", true, serde_json::to_value(&a)?, || {
        format!(
            "onay #{id}: {}",
            if reject { "REDDEDİLDİ" } else { "ONAYLANDI" }
        )
    });
    Ok(true)
}

// ------------------------------- verify m3 -------------------------------

fn check(name: &str, pass: bool, detail: serde_json::Value) -> atlas_sim::validation::CheckOutcome {
    atlas_sim::validation::CheckOutcome {
        name: name.into(),
        pass,
        detail,
    }
}

/// Yerel webhook alıcısı (dayanıklılık testinde gerçek HTTP teslimi için).
fn webhook_sink() -> Result<(String, std::sync::Arc<std::sync::atomic::AtomicUsize>)> {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    let addr = l.local_addr()?;
    let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut s = s;
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let mut need: Option<usize> = None;
            while let Ok(k) = s.read(&mut tmp) {
                if k == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..k]);
                if need.is_none()
                    && let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                {
                    let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
                    let cl = head
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    need = Some(p + 4 + cl);
                }
                if need.is_some_and(|x| buf.len() >= x) {
                    break;
                }
            }
            n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    Ok((format!("http://{addr}/atlas"), n))
}

/// bash'e verilecek yol: Windows'ta ters bölü bash'te kaçış karakteridir → düz bölü.
fn bash_path(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// Kullanılacak bash: `ATLAS_BASH`; Windows'ta Git Bash (System32'deki WSL bash'i Windows yolunu
/// tanımaz); aksi halde `bash`.
fn bash_cmd() -> std::process::Command {
    if let Ok(b) = std::env::var("ATLAS_BASH") {
        return std::process::Command::new(b);
    }
    #[cfg(windows)]
    for c in [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files\Git\usr\bin\bash.exe",
    ] {
        if std::path::Path::new(c).exists() {
            return std::process::Command::new(c);
        }
    }
    std::process::Command::new("bash")
}

fn admin(h: &Home, cmd: &str) -> (i32, String) {
    let exe = std::env::current_exe()
        .map(|p| bash_path(&p))
        .unwrap_or_default();
    let script = bash_path(&h.root.join("deploy").join("atlas-admin"));
    match bash_cmd()
        .arg(&script)
        .env("SSH_ORIGINAL_COMMAND", cmd)
        .env("ATLAS_BIN", &exe)
        .env("ATLAS_ADMIN_HOME", bash_path(&h.root))
        .output()
    {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).to_string(),
        ),
        Err(e) => (-2, e.to_string()),
    }
}

pub fn verify_m3(h: &Home, out: &Out, replay_days: u64) -> Result<bool> {
    let mut checks = Vec::new();

    // 1. Grok izin testleri (sarmalayıcı + CLI çift kontrol)
    let allowed = [
        "status --json",
        "health",
        "events --limit 3",
        "strategies list",
        "experiments summary",
        "budget",
        "approvals list",
        "report daily",
        "strategy pause yok-boyle-strateji test",
        "trade list",
        "market",
    ];
    let forbidden = [
        "approve 1",
        "kill-reset global",
        "gauntlet run trend_pullback",
        "data pull-binance --symbol BTCUSDT --from 2026-01 --to 2026-01",
        "daemon run",
        "status --home /tmp",
        "status; rm -rf /",
        "journal checkpoint",
        "risk set max 1",
        "deploy",
        "verify m3 && echo x",
        "strategies list $(id)",
    ];
    let mut bad = Vec::new();
    for c in allowed {
        let (code, err) = admin(h, c);
        // İzinli komut sarmalayıcıdan geçip CLI'ya ulaşmalı: CLI çıkış kodları 0/1/2.
        if !(0..=2).contains(&code) {
            bad.push(format!("izinli reddedildi: {c} ({code}) {err}"));
        }
    }
    for c in forbidden {
        let (code, _) = admin(h, c);
        if code != 126 {
            bad.push(format!("yasak geçti: {c} ({code})"));
        }
    }
    // atlas-admin atlanıp sudo yardımcısı doğrudan çağrılırsa da (argv düzeyi) yasaklar reddedilir.
    let helper = bash_path(&h.root.join("deploy").join("atlas-grok"));
    for args in [
        vec!["approve", "1"],
        vec!["kill-reset", "global"],
        vec!["daemon", "run"],
        vec!["gauntlet", "run", "x"],
        vec!["risk", "set", "max", "1"],
        vec!["status", "--home", "/tmp"],
        vec!["--home=/tmp", "status"],
        vec!["experiments", "run"],
        vec!["journal", "checkpoint"],
        vec![],
    ] {
        let code = bash_cmd()
            .arg(&helper)
            .args(&args)
            .output()
            .map(|o| o.status.code().unwrap_or(-1))
            .unwrap_or(-2);
        if code != 126 {
            bad.push(format!("yardımcı yasak geçti: {args:?} ({code})"));
        }
    }
    let exe = std::env::current_exe()?;
    for args in [
        vec!["approve", "1"],
        vec!["kill-reset", "global"],
        vec!["--home", ".", "status"],
        vec!["daemon", "run"],
    ] {
        let o = std::process::Command::new(&exe)
            .args(&args)
            .env("ATLAS_ROLE", "grok")
            .env("ATLAS_HOME", &h.root)
            .output()?;
        if o.status.code() != Some(2) || !String::from_utf8_lossy(&o.stderr).contains("izin yok") {
            bad.push(format!("CLI ikinci kontrol başarısız: {args:?}"));
        }
    }
    checks.push(check(
        "grok_permissions",
        bad.is_empty(),
        json!({"allowed": allowed.len(), "forbidden": forbidden.len(), "failures": bad}),
    ));

    // 2. Ajan hattı (sahte sağlayıcı): geçerli hipotez kuyruğa, geçersizler atılır ve sayılır
    {
        let mut reg = Registry::open_in_memory()?;
        let p = atlas_agents::provider::MockProvider::new(vec![
            r#"{"rationale":"r","genome_toml":"name = \"x\"\ninstrument = \"Y\"\n[entry]\nlong = { gt = [\"ema:50\", \"ema:200\"] }\n[exit]\natr = 14\nstop_atr = 3.0\n[sizing]\nrisk_usd = 1\n","expected_behavior":"e","falsification":"f"}"#.into(),
            r#"{"verdict":"onay","why_fake":"w","control_experiment":"c"}"#.into(),
            "çöp".into(),
        ]);
        let mut bt = atlas_risk::BudgetTracker::new(atlas_risk::LlmBudget {
            daily_usd: Money::from_usd(1),
            daily_tokens: 100_000,
        });
        let mut b = atlas_agents::Budgeted {
            provider: &p,
            pricing: atlas_agents::Pricing {
                input_per_mtok: Money::from_usd(3),
                output_per_mtok: Money::from_usd(15),
            },
            budget: &mut bt,
            day: chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap_or_default(),
            now: now(),
            events: vec![],
        };
        let r = atlas_agents::research_cycle(&mut b, &mut reg, "c", "BTCUSDT", 1000, 2)?;
        checks.push(check(
            "agents_schema_validation",
            r.queued.len() == 1 && r.invalid == 1,
            serde_json::to_value(&r)?,
        ));
    }

    // 3. Sapma dedektörü: bozulmuş performans düşürülür
    {
        let exp = atlas_portfolio::Expected {
            mean_r: 0.2,
            sd_r: 1.0,
        };
        let mut m = atlas_portfolio::DriftMonitor::new(exp);
        let mut v = atlas_portfolio::Verdict::Continue;
        for i in 0..200 {
            let r = if i % 2 == 0 { -1.2 } else { 0.4 };
            v = m.update(r).verdict;
            if v != atlas_portfolio::Verdict::Continue {
                break;
            }
        }
        checks.push(check(
            "drift_demotes_degraded",
            v == atlas_portfolio::Verdict::Demote,
            json!({"n": m.results_r.len()}),
        ));
    }

    // 4. Dayanıklılık: aynı daemon kodu, sanal saatle in-sample'ın son N günü (72 sa = 3 gün)
    {
        let dir = tempfile_dir()?;
        let (url, received) = webhook_sink()?;
        let mut opts = DaemonOpts::from_home(h, Some(replay_days), None);
        opts.journal = dir.join("journal.sqlite");
        opts.registry = dir.join("registry.sqlite");
        opts.outbox = dir.join("outbox");
        opts.state = dir.join("ops_state.json");
        opts.webhook_override = Some(url);
        opts.search_every_override = Some(30);
        // Doğrulama koşusu gerçek daemon'a dokunmaz: kendi durdurma dosyası, ajan yok, yerel webhook.
        opts.stop_file = dir.join("daemon.stop");
        opts.disable_agents = true;
        // Bir referans stratejiyi gölgeye koy: koşucu, sapma ve olay yolu gerçek veriyle çalışsın.
        {
            let mut reg = Registry::open(&opts.registry)?;
            let (g, _) = crate::m2::load_genome(h, "trend_pullback")?;
            reg.record(&atlas_registry::ExperimentRecord {
                parent_id: None,
                source: "verify".into(),
                name: g.name.clone(),
                genome_hash: g.hash(),
                genome_json: serde_json::to_string(&g)?,
                tags: vec![],
                data_version: "verify".into(),
                fill_model: "bar_fill@v1".into(),
                seed: 0,
                passed: false,
                failed_gate: Some(0),
                trades: 0,
                sharpe: None,
                report_json: "{}".into(),
                report_md: String::new(),
                cpu_ms: 0,
                llm_tokens: 0,
                created_ts: 0,
            })?;
            reg.set_stage(
                &g.hash(),
                &g.name,
                Stage::Shadow,
                "verify m3 dayanıklılık",
                "verify",
                0,
            )?;
        }
        // Grok operatör hesabı (ADR 0010): bir alış emri — daemon uygulamalı, risk kapısından geçirip
        // doldurmalı ve hesap görüntüsüne yansıtmalı. Stop çok uzakta: adet küçük, risk tavanın altında.
        opts.grok_snapshot = Some(dir.join("grok_account.json"));
        opts.market_snapshot = Some(dir.join("market.json"));
        {
            let mut j = atlas_journal::Journal::open(&opts.journal)?;
            let order = atlas_exec::manual::ManualOrder::Open {
                side: atlas_core::Side::Buy,
                qty: 1,
                limit: None,
                stop: atlas_core::Price(1),
                target: None,
                expiry_min: None,
                reason: "verify m3 grok hesabı".into(),
            };
            j.append(&Event::new(
                now(),
                EventKind::GrokOrder,
                json!({"account": "grok", "order": order, "by": "verify"}),
            ))?;
        }
        let t0 = std::time::Instant::now();
        let s = daemon::run(h, &opts)?;
        let grok: serde_json::Value = std::fs::read_to_string(dir.join("grok_account.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let grok_ok = grok["orders"][0]["status"] == "filled" && grok["position"].is_object();
        let market_ok = dir.join("market.json").is_file();
        let j = atlas_journal::Journal::open(&opts.journal)?;
        let rp = j.replay()?;
        let reg = Registry::open(&opts.registry)?;
        let trials = reg.trial_count()?;
        let got = received.load(std::sync::atomic::Ordering::SeqCst);
        let detail = json!({"summary": s, "seconds": t0.elapsed().as_secs_f64(), "webhook_received": got,
            "journal_consistent": rp.consistent(), "journal_events": rp.events, "trials": trials,
            "market_hours": replay_days * 24, "grok_order_filled": grok_ok, "market_snapshot": market_ok,
            "grok_orders": grok["orders"]});
        let pass = s.outbox.2 == 0
            && grok_ok
            && market_ok
            && s.engine_failures == 0
            && rp.consistent()
            && trials > 1
            && s.runner_steps > 0
            && got > 0
            && s.outbox.0 == 0;
        checks.push(check("soak_72h_replay_unattended", pass, detail));
    }

    // 5. Kurulum dosyaları mevcut ve sözdizimi geçerli
    {
        let mut bad = Vec::new();
        for f in [
            "deploy/atlas-admin",
            "deploy/install.sh",
            "deploy/atlas.service",
            "deploy/logrotate.conf",
            "deploy/atlas-grok",
            "deploy/atlas-mcp.service",
            "deploy/nginx-atlas-mcp.conf",
        ] {
            if !h.root.join(f).exists() {
                bad.push(format!("{f} yok"));
            }
        }
        for f in [
            "deploy/atlas-admin",
            "deploy/install.sh",
            "deploy/atlas-grok",
        ] {
            if let Ok(o) = bash_cmd()
                .arg("-n")
                .arg(bash_path(&h.root.join(f)))
                .output()
                && !o.status.success()
            {
                bad.push(format!("{f}: {}", String::from_utf8_lossy(&o.stderr)));
            }
        }
        checks.push(check(
            "deploy_files",
            bad.is_empty(),
            json!({"problems": bad}),
        ));
    }

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
            json!({"verify": "m3", "passed": passed, "total": checks.len(), "failed": failed}),
        ));
    }
    out.emit("verify.m3", green, json!({"milestone": "m3", "green": green, "passed": passed, "total": checks.len(), "checks": checks,
        "note": "VPS üzerinde gerçek 72 saatlik canlı koşu ayrıca gerekir (§7.1)"}), || {
        let mut s = format!("verify m3: {passed}/{} {}\n", checks.len(), if green { "YEŞİL" } else { "KIRMIZI" });
        for c in &checks {
            let d = c.detail.to_string();
            let d = if d.len() > 300 { format!("{}…", &d[..d.char_indices().nth(300).map_or(d.len(), |x| x.0)]) } else { d };
            s += &format!("  {} {:<34} {}\n", if c.pass { "OK  " } else { "FAIL" }, c.name, d);
        }
        s += "  not: VPS'te gerçek 72 saatlik canlı koşu devir koşulu olarak ayrıca yapılır.";
        s
    });
    Ok(green)
}

fn tempfile_dir() -> Result<std::path::PathBuf> {
    let d = std::env::temp_dir().join(format!("atlas-verify-m3-{}", now().0));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}
