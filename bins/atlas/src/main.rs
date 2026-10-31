//! `atlas` — tek CLI (§6). Grok Bot'un sisteme tek arayüzü; her komut `--json` destekler.
//! Çıktı şeması: docs/cli-schema.md (`atlas.cli/1`).

mod daemon;
mod home;
mod m2;
mod m3;
mod verify;

use anyhow::{Context, Result, anyhow, bail};
use atlas_core::calendar::{format_utc, from_utc, parse_rfc3339};
use atlas_core::{Event, EventKind, Priority, UnixNanos};
use atlas_journal::{EventFilter, Journal};
use atlas_risk::KillScope;
use clap::{Parser, Subcommand};
use home::Home;
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

pub const SCHEMA: &str = "atlas.cli/1";

#[derive(Parser)]
#[command(name = "atlas", version, about = "Atlas araştırma ve işlem platformu")]
struct Cli {
    /// Makine-okunur çıktı (docs/cli-schema.md).
    #[arg(long, global = true)]
    json: bool,
    /// ATLAS_HOME (configs/ içeren kök). Varsayılan: $ATLAS_HOME ya da çalışma dizini.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sistem durumu özeti.
    Status,
    /// Sağlık kontrolü; çıkış kodu 0 = sağlıklı.
    Health,
    /// Olay kuyruğu.
    Events {
        /// RFC3339 zaman (2026-10-31T12:00:00Z) ya da seq:N (N'den sonrakiler).
        #[arg(long)]
        since: Option<String>,
        /// high | medium | info (bu ve daha acil olanlar).
        #[arg(long)]
        prio: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Kilometre taşı kabul testleri.
    Verify {
        /// m1 | m2
        milestone: String,
        /// m2: sahte strateji testindeki aday sayısı (spec: 1000).
        #[arg(long, default_value_t = 1000)]
        fake_n: usize,
    },
    /// Gauntlet: adayı 10 kapıdan geçir (sicile yazılır, deneme sayılır).
    Gauntlet {
        #[command(subcommand)]
        cmd: GauntletCmd,
    },
    /// Merdivendeki stratejiler.
    Strategies {
        #[command(subcommand)]
        cmd: StrategiesCmd,
    },
    /// Deney sicili.
    Experiments {
        #[command(subcommand)]
        cmd: ExperimentsCmd,
    },
    /// Journal işlemleri.
    Journal {
        #[command(subcommand)]
        cmd: JournalCmd,
    },
    /// Veri çekimi ve depo.
    Data {
        #[command(subcommand)]
        cmd: DataCmd,
    },
    /// Sürekli çalışan daemon (yalnız Arman / systemd).
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Raporcu çıktısı + metrikler.
    Report {
        /// daily | weekly
        kind: String,
    },
    /// Strateji yaşam döngüsü (yalnız kısıtlayıcı eylemler).
    Strategy {
        /// pause | demote | resume
        action: String,
        /// Ad ya da genom hash öneki.
        id: String,
        reason: Vec<String>,
    },
    /// LLM / veri / arama bütçesi.
    Budget,
    /// Onay talepleri.
    Approvals {
        #[command(subcommand)]
        cmd: ApprovalsCmd,
    },
    /// Onay (yalnız Arman; ATLAS_APPROVER_KEY).
    Approve {
        id: i64,
        #[arg(long)]
        reject: bool,
    },
    /// Kill switch tetikle: global | account | strategy:<id>.
    Kill { scope: String, reason: Vec<String> },
    /// Kill switch'i aç (yalnız Arman).
    KillReset { scope: String },
    /// Journal'a operatör notu (metin argümanda ya da stdin'de).
    Note { text: Vec<String> },
}

#[derive(Subcommand)]
enum DaemonCmd {
    Run {
        /// Hızlandırılmış: in-sample'ın son N gününü sanal saatle oynat.
        #[arg(long)]
        replay_days: Option<u64>,
        #[arg(long)]
        max_ticks: Option<u64>,
    },
}

#[derive(Subcommand)]
enum ApprovalsCmd {
    List,
    Show { id: i64 },
}

#[derive(Subcommand)]
enum GauntletCmd {
    /// Genom dosyası ya da strategies/<ad>.
    Run {
        genome: String,
        /// Kaynak etiketi (insan | evrim | ajan-X/prompt-vN | referans).
        #[arg(long, default_value = "insan")]
        source: String,
    },
    /// Yalnız temel değerlendirme + süre (sicile yazmaz).
    Eval {
        genome: String,
        /// Ücret ve kayma sıfır (teşhis: maliyet öncesi edge var mı?).
        #[arg(long)]
        frictionless: bool,
    },
}

#[derive(Subcommand)]
enum StrategiesCmd {
    List,
    Show { name: String },
}

#[derive(Subcommand)]
enum ExperimentsCmd {
    Summary,
}

#[derive(Subcommand)]
enum JournalCmd {
    /// Baştan katlayıp checkpoint'leri doğrular.
    Replay,
    /// Tutarlı anlık görüntü (SQLite backup API) → var/exports/<ad>. Var olan dosyanın üzerine yazmaz.
    Export { name: String },
    /// Mevcut durumun hash'iyle checkpoint yaz.
    Checkpoint,
}

#[derive(Subcommand)]
enum DataCmd {
    /// Depodaki veri setleri.
    List,
    /// Binance USDⓈ-M 1m bar çekimi (ücretsiz).
    PullBinance {
        #[arg(long)]
        symbol: String,
        /// YYYY-AA
        #[arg(long)]
        from: String,
        /// YYYY-AA
        #[arg(long)]
        to: String,
    },
    /// Binance işlem akışından efektif spread → önerilen kayma (ücretsiz ölçüm).
    MeasureSlippage {
        #[arg(long)]
        symbol: String,
        /// Son kaç gün (Binance aggTrades zaman araması en fazla son 2 gün).
        #[arg(long, default_value_t = 1)]
        days: u64,
        /// 60 sn'lik örnek pencere sayısı.
        #[arg(long, default_value_t = 200)]
        windows: usize,
    },
    /// Databento maliyet sorgusu + tavan kontrolü (indirme yapmaz).
    QuoteDatabento {
        #[arg(long, default_value = "GLBX.MDP3")]
        dataset: String,
        #[arg(long)]
        symbols: String,
        #[arg(long, default_value = "continuous")]
        stype_in: String,
        #[arg(long, default_value = "ohlcv-1m")]
        schema: String,
        #[arg(long)]
        start: String,
        #[arg(long)]
        end: String,
    },
}

/// Grok'un çalıştıramayacağı komutlar (sarmalayıcıda ve burada çift kontrol, §6).
fn guard_role(cmd: &Cmd) -> Result<()> {
    let role = std::env::var("ATLAS_ROLE").unwrap_or_default();
    if role.eq_ignore_ascii_case("grok") {
        let forbidden = match cmd {
            Cmd::KillReset { .. } => Some("kill-reset"),
            Cmd::Data {
                cmd:
                    DataCmd::PullBinance { .. }
                    | DataCmd::QuoteDatabento { .. }
                    | DataCmd::MeasureSlippage { .. },
            } => Some("data pull/quote"),
            Cmd::Journal {
                cmd: JournalCmd::Checkpoint,
            } => Some("journal checkpoint"),
            Cmd::Gauntlet { .. } => Some("gauntlet (holdout bütçesi ve deneme sayacı tüketir)"),
            Cmd::Approve { .. } => Some("approve (yalnız Arman)"),
            Cmd::Daemon { .. } => Some("daemon"),
            _ => None,
        };
        if let Some(f) = forbidden {
            bail!("izin yok: '{f}' yalnız Arman (ATLAS_ROLE=grok)");
        }
    }
    Ok(())
}

pub fn now() -> UnixNanos {
    from_utc(chrono::Utc::now())
}

/// Ortak çıktı: JSON zarfı ya da insan-okur metin.
pub struct Out {
    pub json: bool,
}

impl Out {
    pub fn emit(&self, command: &str, ok: bool, data: Value, human: impl FnOnce() -> String) {
        if self.json {
            let v = json!({"schema": SCHEMA, "command": command, "ok": ok, "data": data});
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        } else {
            println!("{}", human());
        }
    }
}

fn open_journal(h: &Home) -> Result<Journal> {
    let p = h.journal_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    Journal::open(&p).with_context(|| format!("journal açılamadı: {}", p.display()))
}

fn event_json(seq: i64, e: &Event) -> Value {
    json!({"seq": seq, "id": e.id, "ts": e.ts.0, "time": format_utc(e.ts), "kind": e.kind,
           "priority": e.priority, "key": e.key, "payload": e.payload})
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let out = Out { json: cli.json };
    match run(cli, &out) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            if out.json {
                let v = json!({"schema": SCHEMA, "ok": false, "error": format!("{e:#}")});
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                eprintln!("HATA: {e:#}");
            }
            ExitCode::from(2)
        }
    }
}

fn is_grok() -> bool {
    std::env::var("ATLAS_ROLE").is_ok_and(|r| r.eq_ignore_ascii_case("grok"))
}

/// Journal'dan gelen metni terminale basmadan önce kontrol karakterlerinden arındırır.
pub fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(500).collect()
}

/// Girdi metni: kontrol karakteri yok, en fazla `max` karakter.
fn clean_input(s: &str, max: usize) -> Result<String> {
    let t = s.trim();
    if t.chars().any(char::is_control) {
        bail!("metin kontrol karakteri içeremez");
    }
    if t.chars().count() > max {
        bail!("metin en fazla {max} karakter");
    }
    Ok(t.to_string())
}

fn run(cli: Cli, out: &Out) -> Result<bool> {
    guard_role(&cli.cmd)?;
    if is_grok() && cli.home.is_some() {
        bail!("izin yok: --home yalnız Arman (Grok'un ATLAS_HOME'u sarmalayıcıda sabittir)");
    }
    let h = Home::locate(cli.home)?;
    match cli.cmd {
        Cmd::Status => status(&h, out),
        Cmd::Health => health(&h, out),
        Cmd::Events {
            since,
            prio,
            kind,
            limit,
        } => events(&h, out, since, prio, kind, limit),
        Cmd::Gauntlet { cmd } => match cmd {
            GauntletCmd::Run { genome, source } => m2::gauntlet_run(&h, out, &genome, &source),
            GauntletCmd::Eval {
                genome,
                frictionless,
            } => m2::gauntlet_eval(&h, out, &genome, frictionless),
        },
        Cmd::Strategies { cmd } => match cmd {
            StrategiesCmd::List => m2::strategies_list(&h, out),
            StrategiesCmd::Show { name } => m2::strategies_show(&h, out, &name),
        },
        Cmd::Experiments {
            cmd: ExperimentsCmd::Summary,
        } => m2::experiments_summary(&h, out),
        Cmd::Verify { milestone, fake_n } => match milestone.as_str() {
            "m2" => m2::verify_m2(&h, out, fake_n),
            "m3" => m3::verify_m3(&h, out, 3),
            "m1" => verify::m1(&h, out),
            other => bail!("{other}: henüz tanımlı değil (M1 aşamasındayız)"),
        },
        Cmd::Journal { cmd } => journal(&h, out, cmd),
        Cmd::Data { cmd } => data(&h, out, cmd),
        Cmd::Daemon {
            cmd:
                DaemonCmd::Run {
                    replay_days,
                    max_ticks,
                },
        } => m3::daemon_run(&h, out, replay_days, max_ticks),
        Cmd::Report { kind } => m3::report(&h, out, &kind),
        Cmd::Strategy { action, id, reason } => {
            m3::strategy(&h, out, &action, &id, &reason.join(" "))
        }
        Cmd::Budget => m3::budget(&h, out),
        Cmd::Approvals { cmd } => match cmd {
            ApprovalsCmd::List => m3::approvals(&h, out, None),
            ApprovalsCmd::Show { id } => m3::approvals(&h, out, Some(id)),
        },
        Cmd::Approve { id, reject } => m3::approve(&h, out, id, reject),
        Cmd::Kill { scope, reason } => kill(&h, out, &scope, &reason.join(" ")),
        Cmd::KillReset { scope } => kill_reset(&h, out, &scope),
        Cmd::Note { text } => note(&h, out, text),
    }
}

fn prop_status(h: &Home) -> Value {
    match h.prop() {
        Ok(Ok(r)) => {
            json!({"file": h.cfg.account.prop, "loaded": true, "firm": r.firm, "account": r.account_label})
        }
        Ok(Err(e)) => json!({"file": h.cfg.account.prop, "loaded": false, "error": e.to_string()}),
        Err(e) => json!({"file": h.cfg.account.prop, "loaded": false, "error": format!("{e:#}")}),
    }
}

fn data_summary(h: &Home) -> Value {
    match atlas_data::Store::new(h.store_path()).manifest() {
        Ok(m) => {
            let mut per: std::collections::BTreeMap<String, (u64, u64, String, String)> =
                std::collections::BTreeMap::new();
            for e in m.entries.values() {
                let k = format!("{}/{}/{}", e.source, e.symbol, e.schema);
                let x = per
                    .entry(k)
                    .or_insert((0, 0, e.month.clone(), e.month.clone()));
                x.0 += e.count;
                x.1 += e.quality_issues;
                if e.month < x.2 {
                    x.2 = e.month.clone();
                }
                if e.month > x.3 {
                    x.3 = e.month.clone();
                }
            }
            json!(per
                .into_iter()
                .map(|(k, (n, q, a, b))| json!({"dataset": k, "bars": n, "quality_issues": q, "from": a, "to": b}))
                .collect::<Vec<_>>())
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

fn status(h: &Home, out: &Out) -> Result<bool> {
    let j = open_journal(h)?;
    let p = j.projection().clone();
    let last = j.last()?;
    let data = json!({
        "time": format_utc(now()),
        "journal": {"events": j.count()?, "last": last.as_ref().map(|s| event_json(s.seq, &s.event))},
        "positions": p.positions,
        "realized_net_usd": p.realized_net_micros as f64 / 1e6,
        "kills": p.kills,
        "prop": prop_status(h),
        "data": data_summary(h),
        "milestone": "M1",
    });
    out.emit("status", true, data.clone(), || {
        let mut s = format!("Atlas durum — {}\n", format_utc(now()));
        s += &format!("  journal: {} olay\n", data["journal"]["events"]);
        s += &format!("  açık pozisyon: {}\n", p.positions.len());
        s += &format!(
            "  gerçekleşmiş net: ${:.2}\n",
            p.realized_net_micros as f64 / 1e6
        );
        if p.kills.is_empty() {
            s += "  kill switch: yok\n";
        } else {
            for (k, r) in &p.kills {
                s += &format!("  KILL {}: {}\n", printable(k), printable(r));
            }
        }
        s += &format!("  prop config: {}\n", data["prop"]);
        s += &format!("  veri: {}", data["data"]);
        s
    });
    Ok(true)
}

fn health(h: &Home, out: &Out) -> Result<bool> {
    let mut checks: Vec<(String, bool, String)> = Vec::new();
    let mut add = |n: &str, r: Result<String>| match r {
        Ok(d) => checks.push((n.into(), true, d)),
        Err(e) => checks.push((n.into(), false, format!("{e:#}"))),
    };
    add(
        "instruments",
        h.instruments().map(|t| format!("{} enstrüman", t.len())),
    );
    add(
        "calendar",
        h.calendar().map(|c| format!("{} seans", c.names().count())),
    );
    add("risk_policy", h.policy().map(|_| "yüklendi".into()));
    add(
        "prop_config",
        h.prop().and_then(|r| {
            r.map(|p| format!("{} {}", p.firm, p.account_label))
                .map_err(|e| anyhow!("{e}"))
        }),
    );
    add(
        "journal",
        open_journal(h).and_then(|j| {
            let r = j.replay()?;
            if r.consistent() {
                Ok(format!(
                    "{} olay, {} checkpoint, tutarlı",
                    r.events, r.checkpoints
                ))
            } else {
                Err(anyhow!("replay tutarsız: {:?}", r.mismatches))
            }
        }),
    );
    add(
        "data_store",
        atlas_data::Store::new(h.store_path())
            .manifest()
            .map(|m| format!("{} dosya", m.entries.len()))
            .map_err(|e| anyhow!("{e}")),
    );
    let ok = checks.iter().all(|c| c.1);
    let data = json!({"healthy": ok, "checks": checks.iter().map(|(n, p, d)| json!({"name": n, "ok": p, "detail": d})).collect::<Vec<_>>()});
    out.emit("health", ok, data, || {
        let mut s = format!("sağlık: {}\n", if ok { "SAĞLIKLI" } else { "SORUNLU" });
        for (n, p, d) in &checks {
            s += &format!("  {} {n}: {d}\n", if *p { "OK  " } else { "FAIL" });
        }
        s.trim_end().to_string()
    });
    Ok(ok)
}

fn events(
    h: &Home,
    out: &Out,
    since: Option<String>,
    prio: Option<String>,
    kind: Option<String>,
    limit: usize,
) -> Result<bool> {
    let j = open_journal(h)?;
    let mut f = EventFilter {
        limit: Some(limit),
        ..Default::default()
    };
    if let Some(s) = since {
        if let Some(n) = s.strip_prefix("seq:") {
            f.after_seq = Some(n.parse().context("seq:N")?);
        } else {
            f.since_ts = Some(parse_rfc3339(&s).map_err(|e| anyhow!("{e}"))?);
        }
    }
    if let Some(p) = prio {
        f.min_priority = Some(p.parse::<Priority>().map_err(|e| anyhow!(e))?);
    }
    if let Some(k) = kind {
        f.kind = Some(k.parse::<EventKind>().map_err(|e| anyhow!(e))?);
    }
    let evs = j.read(&f)?;
    let data = json!(
        evs.iter()
            .map(|s| event_json(s.seq, &s.event))
            .collect::<Vec<_>>()
    );
    out.emit("events", true, data, || {
        if evs.is_empty() {
            return "olay yok".into();
        }
        evs.iter()
            .map(|s| {
                format!(
                    "#{:<6} {} {:<6} {:<22} {}",
                    s.seq,
                    format_utc(s.event.ts),
                    s.event.priority,
                    s.event.kind,
                    s.event.payload
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(true)
}

fn journal(h: &Home, out: &Out, cmd: JournalCmd) -> Result<bool> {
    match cmd {
        JournalCmd::Replay => {
            let j = open_journal(h)?;
            let r = j.replay()?;
            let ok = r.consistent();
            out.emit("journal.replay", ok, serde_json::to_value(&r)?, || {
                format!(
                    "replay: {} olay, {} checkpoint, {} — hash {}",
                    r.events,
                    r.checkpoints,
                    if ok { "TUTARLI" } else { "TUTARSIZ" },
                    &r.final_hash[..16.min(r.final_hash.len())]
                )
            });
            Ok(ok)
        }
        JournalCmd::Export { name } => {
            // Yalnız düz dosya adı; dizin ayırıcı, '..' ve var olan dosya reddedilir.
            let ok_name = !name.is_empty()
                && name.len() <= 100
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && !name.starts_with('.');
            if !ok_name {
                bail!("geçersiz ad {name:?}: yalnız [A-Za-z0-9._-], dizin yok");
            }
            let dir = h.path("var/exports");
            std::fs::create_dir_all(&dir)?;
            let dest = dir.join(&name);
            if dest.exists() {
                bail!("{} zaten var; üzerine yazılmaz", dest.display());
            }
            let j = open_journal(h)?;
            j.export_snapshot(&dest)?;
            out.emit("journal.export", true, json!({"dest": dest}), || {
                format!("anlık görüntü yazıldı: {}", dest.display())
            });
            Ok(true)
        }
        JournalCmd::Checkpoint => {
            let mut j = open_journal(h)?;
            let seq = j.checkpoint(now())?;
            let hash = j.projection().hash();
            out.emit(
                "journal.checkpoint",
                true,
                json!({"seq": seq, "hash": hash}),
                || format!("checkpoint #{seq} {hash}"),
            );
            Ok(true)
        }
    }
}

fn data(h: &Home, out: &Out, cmd: DataCmd) -> Result<bool> {
    match cmd {
        DataCmd::List => {
            let d = data_summary(h);
            out.emit("data.list", true, d.clone(), || {
                serde_json::to_string_pretty(&d).unwrap_or_default()
            });
            Ok(true)
        }
        DataCmd::PullBinance { symbol, from, to } => {
            let table = h.instruments()?;
            let inst = table.get(&symbol).map_err(|e| anyhow!("{e}"))?;
            let cal = h.calendar()?;
            let session = cal.session(&inst.session).map_err(|e| anyhow!("{e}"))?;
            let api = atlas_data::binance::BinanceFutures::new()?;
            let store = atlas_data::Store::new(h.store_path());
            let rep =
                atlas_data::binance::pull_months(&api, &store, inst, session, &from, &to, now())?;
            let mut j = open_journal(h)?;
            j.append_all(&rep.quality_events)?;
            let issues: u64 = rep.months.iter().map(|m| m.quality_issues).sum();
            let bars: u64 = rep.months.iter().map(|m| m.count).sum();
            out.emit(
                "data.pull_binance",
                true,
                json!({"symbol": symbol, "months": rep.months, "bars": bars, "quality_issues": issues}),
                || {
                    let mut s = format!("{symbol}: {} ay, {bars} bar, {issues} kalite bulgusu\n", rep.months.len());
                    for m in &rep.months {
                        s += &format!("  {} {} bar, sürüm {}, bulgu {}\n", m.month, m.count, m.version.short(), m.quality_issues);
                    }
                    s.trim_end().to_string()
                },
            );
            Ok(true)
        }
        DataCmd::MeasureSlippage {
            symbol,
            days,
            windows,
        } => {
            let table = h.instruments()?;
            let inst = table.get(&symbol).map_err(|e| anyhow!("{e}"))?;
            let api = atlas_data::binance::BinanceFutures::new()?;
            let rep = api.measure_spread(inst, now(), days, windows)?;
            let mut j = open_journal(h)?;
            j.append(&Event::new(
                now(),
                EventKind::Calibration,
                json!({"kind": "slippage", "report": rep}),
            ))?;
            out.emit("data.measure_slippage", true, serde_json::to_value(&rep)?, || {
                format!(
                    "{}: {} örnek ({} pencere, {} → {})\n  spread medyan {:.2} tick, ort {:.3}, p90 {:.2}\n  yarım spread ort {:.3} tick → önerilen kayma {} tick",
                    rep.symbol, rep.samples, rep.windows, rep.from, rep.to,
                    rep.median_spread_ticks, rep.mean_spread_ticks, rep.p90_spread_ticks,
                    rep.mean_half_spread_ticks, rep.recommended_slippage_ticks
                )
            });
            Ok(true)
        }
        DataCmd::QuoteDatabento {
            dataset,
            symbols,
            stype_in,
            schema,
            start,
            end,
        } => {
            use atlas_data::databento::{CostQuoter, DatabentoQuoter, PullRequest, guard};
            let cap = h.data_cap()?;
            let q = DatabentoQuoter::from_env()?;
            let req = PullRequest {
                dataset,
                symbols,
                stype_in,
                schema,
                start,
                end,
            };
            let quote = q.quote(&req)?;
            let ok = guard(quote, cap).is_ok();
            out.emit(
                "data.quote_databento",
                ok,
                json!({"quote_usd": quote.as_usd_f64(), "cap_usd": cap.as_usd_f64(), "within_cap": ok}),
                || format!("maliyet {quote}, tavan {cap}: {}", if ok { "tavan içinde" } else { "ONAY GEREKLİ" }),
            );
            Ok(ok)
        }
    }
}

fn kill(h: &Home, out: &Out, scope: &str, reason: &str) -> Result<bool> {
    let reason = clean_input(reason, 500)?;
    if reason.is_empty() {
        bail!("gerekçe zorunlu: atlas kill <scope> <gerekçe>");
    }
    let s = KillScope::parse(scope).map_err(|e| anyhow!(e))?;
    let mut j = open_journal(h)?;
    if j.projection().kills.contains_key(&s.label()) {
        out.emit(
            "kill",
            true,
            json!({"scope": s.label(), "already_active": true}),
            || format!("{} zaten tetikli", s.label()),
        );
        return Ok(true);
    }
    let ev = Event::new(
        now(),
        EventKind::KillSwitch,
        json!({"scope": s.label(), "reason": reason, "by": std::env::var("ATLAS_ROLE").unwrap_or_else(|_| "arman".into())}),
    )
    .with_key(format!("kill:{}", s.label()));
    let seq = j.append(&ev)?;
    out.emit(
        "kill",
        true,
        json!({"scope": s.label(), "seq": seq}),
        || {
            format!(
                "KILL {} tetiklendi (#{seq}). Açmak yalnız Arman: atlas kill-reset {}",
                s.label(),
                s.label()
            )
        },
    );
    Ok(true)
}

fn kill_reset(h: &Home, out: &Out, scope: &str) -> Result<bool> {
    let s = KillScope::parse(scope).map_err(|e| anyhow!(e))?;
    let mut j = open_journal(h)?;
    if !j.projection().kills.contains_key(&s.label()) {
        bail!("{} tetikli değil", s.label());
    }
    let seq = j.append(&Event::new(
        now(),
        EventKind::KillReset,
        json!({"scope": s.label()}),
    ))?;
    out.emit(
        "kill_reset",
        true,
        json!({"scope": s.label(), "seq": seq}),
        || format!("{} açıldı (#{seq})", s.label()),
    );
    Ok(true)
}

fn note(h: &Home, out: &Out, text: Vec<String>) -> Result<bool> {
    let mut t = text.join(" ");
    if t.trim().is_empty() {
        std::io::stdin().read_to_string(&mut t)?;
    }
    let t = clean_input(&t.replace(['\n', '\r', '\t'], " "), 4000)?;
    if t.is_empty() {
        bail!("not boş");
    }
    let mut j = open_journal(h)?;
    let seq = j.append(&Event::new(
        now(),
        EventKind::OperatorNote,
        json!({"text": t, "by": std::env::var("ATLAS_ROLE").unwrap_or_else(|_| "arman".into())}),
    ))?;
    out.emit("note", true, json!({"seq": seq}), || {
        format!("not yazıldı (#{seq})")
    });
    Ok(true)
}
