//! `atlas daemon run` — tek, sürekli çalışan süreç (ders §9.1).
//!
//! Her dakika: veri akışı (boşluk telafili) → canlı koşucular (gölge/paper) → sapma kararları
//! (terfi/düşürme/onay talebi) → kill switch senkronu → arama döngüsü → (bütçe varsa) ajanlar →
//! heartbeat → olay hunisi (journal → uyandırma → outbox) → webhook teslimi → checkpoint/kalıcılık.
//!
//! `--replay-days N`: aynı kod, sanal saatle depodaki **in-sample** barların son N günü üzerinde
//! hızlandırılmış koşar (holdout'a dokunmaz, kural 8). M3 kabulündeki 72 saatlik dayanıklılık testi
//! bunu kullanır.

use crate::home::Home;
use crate::m2::{gauntlet_cfg, load_genome, load_market};
use crate::now;
use anyhow::{Context as _, Result, anyhow};
use atlas_core::units::{NANOS_PER_DAY, NANOS_PER_MIN};
use atlas_core::{Bar, Event, EventKind, InstrumentId, MarketEvent, MarketPayload, UnixNanos};
use atlas_exec::{LiveFeed, ReplayFeed, Runner};
use atlas_features::FeatureStore;
use atlas_gauntlet::eval::{Cost, Evaluator};
use atlas_gauntlet::{Context, GauntletReport};
use atlas_ops::Sink;
use atlas_ops::outbox::{HttpSender, Outbox, Sender};
use atlas_ops::policy::{WakeConfig, WakePolicy};
use atlas_portfolio::{Expected, Verdict};
use atlas_registry::Registry;
use atlas_registry::ladder::Stage;
use atlas_risk::{KillScope, RiskEnv, RiskGate};
use atlas_search::{SearchConfig, Searcher, screen_market};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct M3Toml {
    #[serde(default)]
    pub webhook: WebhookCfg,
    #[serde(default)]
    pub wake: WakeConfig,
    #[serde(default)]
    pub agents: AgentsCfg,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub daemon: DaemonCfg,
    #[serde(default)]
    pub approvals: ApprovalsCfg,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WebhookCfg {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentsCfg {
    pub provider: String,
    pub model: String,
    pub input_usd_per_mtok: String,
    pub output_usd_per_mtok: String,
    pub daily_usd: String,
    pub daily_tokens: u64,
    pub research_every_minutes: u64,
    pub hypotheses_per_cycle: usize,
}

impl Default for AgentsCfg {
    fn default() -> Self {
        AgentsCfg {
            provider: "none".into(),
            model: String::new(),
            input_usd_per_mtok: String::new(),
            output_usd_per_mtok: String::new(),
            daily_usd: "0".into(),
            daily_tokens: 0,
            research_every_minutes: 60,
            hypotheses_per_cycle: 2,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DaemonCfg {
    pub symbols: Vec<String>,
    pub heartbeat_minutes: u64,
    pub search_every_minutes: u64,
    pub persist_every_minutes: u64,
    pub checkpoint_every_minutes: u64,
    pub warmup_days: u64,
    pub max_clock_skew_ms: u64,
}

impl Default for DaemonCfg {
    fn default() -> Self {
        DaemonCfg {
            symbols: vec!["BTCUSDT".into()],
            heartbeat_minutes: 5,
            search_every_minutes: 1,
            persist_every_minutes: 60,
            checkpoint_every_minutes: 60,
            warmup_days: 3,
            max_clock_skew_ms: 2000,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ApprovalsCfg {
    /// Arman'ın onay anahtarının SHA-256'sı (hex). Boşsa onay komutu kapalı.
    #[serde(default)]
    pub key_sha256: String,
}

pub fn m3_toml(h: &Home) -> Result<M3Toml> {
    toml::from_str(&h.atlas_toml_text).context("atlas.toml (M3 bölümleri)")
}

#[derive(Debug, Clone)]
pub struct DaemonOpts {
    pub replay_days: Option<u64>,
    pub max_ticks: Option<u64>,
    pub journal: PathBuf,
    pub registry: PathBuf,
    pub outbox: PathBuf,
    pub state: PathBuf,
    pub webhook_override: Option<String>,
    pub search_every_override: Option<u64>,
    pub stop_file: PathBuf,
    /// `true`: config ne derse desin LLM çağrısı yapılmaz (doğrulama koşuları).
    pub disable_agents: bool,
}

impl DaemonOpts {
    pub fn from_home(h: &Home, replay_days: Option<u64>, max_ticks: Option<u64>) -> DaemonOpts {
        DaemonOpts {
            replay_days,
            max_ticks,
            journal: h.journal_path(),
            registry: crate::m2::registry_path(h),
            outbox: h.path("var/outbox"),
            state: h.path("var/ops_state.json"),
            webhook_override: None,
            search_every_override: None,
            stop_file: h.path("var/daemon.stop"),
            disable_agents: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DaemonSummary {
    pub ticks: u64,
    pub bars: u64,
    pub events: u64,
    pub screened: u64,
    pub gauntlet_runs: u64,
    pub gauntlet_passed: u64,
    pub runner_steps: u64,
    pub runner_trades: u64,
    pub promotions: u64,
    pub demotions: u64,
    pub approval_requests: u64,
    pub engine_failures: u64,
    pub feed_down: u64,
    pub outbox: (usize, usize, usize),
    pub delivered: usize,
    pub webhook_configured: bool,
    pub stopped_by: String,
}

fn expected_for(reg: &Registry, hash: &str, risk_usd: f64) -> Expected {
    let rep: Option<GauntletReport> = reg
        .last_gauntlet_report_json(hash)
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str(&j).ok());
    match rep {
        Some(r) if r.sharpe.abs() > 1e-9 => {
            let m = r.expectancy_usd / risk_usd;
            Expected {
                mean_r: m,
                sd_r: (m / r.sharpe).abs().max(1e-6),
            }
        }
        _ => Expected {
            mean_r: 0.0,
            sd_r: 1.0,
        },
    }
}

#[cfg(unix)]
fn sd_notify(msg: &str) {
    if let Ok(path) = std::env::var("NOTIFY_SOCKET")
        && let Ok(sock) = std::os::unix::net::UnixDatagram::unbound()
    {
        let _ = sock.send_to(msg.as_bytes(), path);
    }
}

#[cfg(not(unix))]
fn sd_notify(_msg: &str) {}

/// Daemon'u çalıştırır. Canlı modda sonsuza kadar (durdurma dosyası `var/daemon.stop` ya da sinyal).
pub fn run(h: &Home, opts: &DaemonOpts) -> Result<DaemonSummary> {
    let cfg = m3_toml(h)?;
    let gcfg = gauntlet_cfg(h)?;
    let table = h.instruments()?;
    let cal = h.calendar()?;
    let prop = h.prop()?.map_err(|e| anyhow!("{e}"))?;
    let policy = h.policy()?;
    let prop_gate = Arc::new(RiskGate::new(RiskEnv {
        rules: prop.clone(),
        policy,
        instruments: table.clone(),
        calendar: cal.clone(),
    }));
    let research_gate = Arc::new(RiskGate::new(atlas_gauntlet::eval::research_env(
        &table, &cal,
    )?));
    for p in [&opts.journal, &opts.registry, &opts.state] {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
    }
    let mut sink = Sink::new(
        atlas_journal::Journal::open(&opts.journal)?,
        WakePolicy::new(cfg.wake.clone()),
        Outbox::open(&opts.outbox)?,
        opts.state.clone(),
    )?;
    let url = opts
        .webhook_override
        .clone()
        .unwrap_or(cfg.webhook.url.clone());
    let sender = HttpSender::new(&url);
    let mut reg = Registry::open(&opts.registry)?;
    let store = atlas_data::Store::new(h.store_path());
    let symbol = cfg
        .daemon
        .symbols
        .first()
        .cloned()
        .unwrap_or_else(|| "BTCUSDT".into());
    let inst = table.get(&symbol).map_err(|e| anyhow!("{e}"))?.clone();
    let session = cal
        .session(&inst.session)
        .map_err(|e| anyhow!("{e}"))?
        .clone();

    // In-sample (holdout hariç) piyasa; replay modunda son N gün canlı akış olur, arama geri kalanı kullanır.
    let loaded = load_market(h, &symbol, &gcfg)?;
    let mut is_bars: Vec<Bar> = loaded.market.bars.as_ref().clone();
    let replay = opts.replay_days.is_some();
    let (mut feed, start): (Box<dyn LiveFeed>, UnixNanos) = match opts.replay_days {
        Some(days) => {
            let last = is_bars.last().map_or(UnixNanos::ZERO, |b| b.close_ts);
            let from = last.minus(days * NANOS_PER_DAY);
            let cut = is_bars.partition_point(|b| b.open_ts < from);
            let live = is_bars.split_off(cut);
            (Box::new(ReplayFeed::new(&symbol, inst.id, live)), from)
        }
        None => {
            let last = is_bars.last().map_or(UnixNanos::ZERO, |b| b.close_ts);
            let api = atlas_data::binance::BinanceFutures::new()?;
            // Holdout dönemi depoda; canlı akış depodaki son bardan devam eder (boşluk telafisi).
            let (all, _) =
                store.load_bars("binance_futures", &symbol, "ohlcv_1m", last, UnixNanos::MAX)?;
            let resume = all.last().map_or(last, |b| b.close_ts);
            (
                Box::new(atlas_exec::BinanceKlineFeed::new(api, inst.clone(), resume)),
                now(),
            )
        }
    };
    let market = atlas_gauntlet::eval::Market {
        data_version: format!("{}-daemon", loaded.market.data_version),
        bars: Arc::new(is_bars.clone()),
        ..loaded.market.clone()
    };
    let short = screen_market(&market, cfg.search.screen_days);
    let fstore = FeatureStore::new();
    let ev = Evaluator::new(atlas_gauntlet::eval::research_env(&table, &cal)?, &fstore);
    let ctx = Context {
        eval: &ev,
        market: &market,
        vault: &loaded.vault,
        prop: &prop,
        cfg: &gcfg,
        seed: h.cfg.verify.seed,
        cost: Cost::BASE,
    };
    let seeds: Vec<atlas_genome::Genome> = crate::m2::REFERENCE
        .iter()
        .filter_map(|n| load_genome(h, n).ok().map(|x| x.0))
        .collect();
    let mut searcher = Searcher::new(h.cfg.verify.seed, &symbol, cfg.search.clone(), seeds);
    searcher.load_source_priors(&reg)?;

    // Isınma + canlı barların geçmişi (koşucular yeniden kurulurken kullanılır).
    let warm_from = start.minus(cfg.daemon.warmup_days * NANOS_PER_DAY);
    let mut history: Vec<Bar> = is_bars
        .iter()
        .filter(|b| b.open_ts >= warm_from)
        .copied()
        .collect();
    let mut unsaved: Vec<Bar> = Vec::new();
    let mut runners: BTreeMap<String, Runner> = BTreeMap::new();
    let mut sum = DaemonSummary {
        webhook_configured: sender.configured(),
        ..Default::default()
    };
    let search_every = opts
        .search_every_override
        .unwrap_or(cfg.daemon.search_every_minutes)
        .max(1);
    let stop_file = opts.stop_file.clone();
    let mut clock = UnixNanos(start.0 - start.0 % NANOS_PER_MIN);
    let mut last_agent = UnixNanos::ZERO;
    sink.emit(
        &[Event::new(
            clock,
            EventKind::ConfigLoaded,
            json!({"daemon": "başladı", "symbol": symbol, "replay": opts.replay_days,
            "webhook": sender.configured(), "agents": cfg.agents.provider}),
        )],
        clock,
    )?;
    sd_notify("READY=1");

    loop {
        let t = if replay { clock } else { now() };
        sum.ticks += 1;
        let mut events: Vec<Event> = Vec::new();

        // 1. Veri
        let new = feed.poll(t);
        let mut new_bars: Vec<MarketEvent> = Vec::new();
        for e in new {
            match &e.payload {
                MarketPayload::Bar(b) => {
                    history.push(*b);
                    if !replay {
                        unsaved.push(*b);
                    }
                    sum.bars += 1;
                    new_bars.push(e);
                }
                MarketPayload::Feed(atlas_core::FeedStatus::Down { reason }) => {
                    sum.feed_down += 1;
                    events.push(
                        Event::new(
                            t,
                            EventKind::FeedDown,
                            json!({"symbol": symbol, "reason": reason}),
                        )
                        .with_key(format!("feed_down:{symbol}")),
                    );
                    new_bars.push(e);
                }
                _ => new_bars.push(e),
            }
        }
        if !new_bars.is_empty() {
            let bars: Vec<Bar> = new_bars
                .iter()
                .filter_map(|e| {
                    if let MarketPayload::Bar(b) = &e.payload {
                        Some(*b)
                    } else {
                        None
                    }
                })
                .collect();
            for q in atlas_data::quality::check_bars(&bars, &session, NANOS_PER_MIN) {
                events.push(q.to_event(&symbol, t));
            }
        }
        // Isınma tamponunu sınırla.
        let keep_from = t.minus(cfg.daemon.warmup_days * NANOS_PER_DAY);
        if history.first().is_some_and(|b| b.open_ts < keep_from) && history.len() > 20_000 {
            let cut = history.partition_point(|b| b.open_ts < keep_from);
            history.drain(..cut);
        }

        // 2. Koşucu senkronu (sicildeki canlı aşamalar)
        let stages = reg.all_current_stages()?;
        let live: BTreeMap<String, (Stage, u64, String)> = stages
            .iter()
            .filter(|s| s.stage.is_live())
            .map(|s| (s.genome_hash.clone(), (s.stage, s.ts, s.name.clone())))
            .collect();
        runners.retain(|k, r| live.get(k).is_some_and(|(st, _, _)| *st == r.stage));
        for (hash, (stage, ts, name)) in &live {
            if runners.contains_key(hash) {
                continue;
            }
            let Some(gj) = reg.genome_json_for_hash(hash)? else {
                continue;
            };
            let Ok(g) = serde_json::from_str::<atlas_genome::Genome>(&gj) else {
                continue;
            };
            if g.instrument != symbol {
                continue;
            }
            let gate = if *stage == Stage::Shadow {
                Arc::clone(&research_gate)
            } else {
                Arc::clone(&prop_gate)
            };
            let live_from = UnixNanos(*ts).max(start);
            let exp = expected_for(&reg, hash, f64::from(g.sizing.risk_usd));
            match Runner::new(
                &g,
                &inst,
                session.clone(),
                gate,
                *stage,
                live_from,
                warm_from,
                exp,
            ) {
                Ok(mut r) => {
                    // Isınma: geçmiş barlar (bu tick'in barları hariç) yeniden oynatılır.
                    let upto = new_bars.first().map_or(t, |e| e.ts);
                    for b in history.iter().filter(|b| b.close_ts < upto) {
                        let _ = r.step(&MarketEvent::bar(InstrumentId(inst.id.0), *b));
                    }
                    events.push(Event::new(
                        t,
                        EventKind::StageChange,
                        json!({"runner": "başladı", "strategy": name, "stage": stage}),
                    ));
                    runners.insert(hash.clone(), r);
                }
                Err(e) => events.push(Event::new(
                    t,
                    EventKind::EngineFailure,
                    json!({"strategy": name, "error": e.to_string()}),
                )),
            }
        }

        // 3. Kill switch senkronu (CLI `kill` / `kill-reset` journal'a yazar).
        // Her koşucunun kendi (araştırma/paper) hesabı vardır: global/account kill hepsine, `strategy:X`
        // yalnız adı X olan ya da genom hash'i X ile başlayan koşucuya, o koşucunun hesap kill'i olarak uygulanır.
        let kills = sink.journal.projection().kills.clone();
        for (hash, r) in runners.iter_mut() {
            let name = r.name.clone();
            let wanted: BTreeMap<String, String> = kills
                .iter()
                .filter_map(|(scope, reason)| match KillScope::parse(scope) {
                    Ok(KillScope::Global) => Some(("global".to_string(), reason.clone())),
                    Ok(KillScope::Account) => Some(("account".to_string(), reason.clone())),
                    Ok(KillScope::Strategy(id)) => {
                        let id = id.as_str();
                        (id == name || (id.len() >= 6 && hash.starts_with(id)))
                            .then(|| ("account".to_string(), format!("strategy:{id} — {reason}")))
                    }
                    Err(_) => None,
                })
                .collect();
            let ks = &mut r.engine_mut().state_mut().kills;
            let active: Vec<String> = ks.active().into_iter().map(|(s, _)| s.label()).collect();
            for (label, reason) in &wanted {
                if !active.contains(label)
                    && let Ok(sc) = KillScope::parse(label)
                {
                    ks.trigger(sc, reason, t);
                }
            }
            for a in active {
                if !wanted.contains_key(&a)
                    && let Ok(sc) = KillScope::parse(&a)
                {
                    ks.reset(&sc, t);
                }
            }
        }

        // 4. Koşucuları adımla, sapma kararları
        let mut failed: Vec<String> = Vec::new();
        let mut decisions: Vec<(String, String, Stage, Verdict, serde_json::Value)> = Vec::new();
        for (hash, r) in runners.iter_mut() {
            for e in &new_bars {
                sum.runner_steps += 1;
                match r.step(e) {
                    Ok(rep) => {
                        sum.runner_trades += rep.new_trades.len() as u64;
                        if r.stage == Stage::Shadow {
                            events.extend(r.shadow_events(&rep.new_trades, t));
                        } else {
                            events.extend(rep.events);
                        }
                        if let Some(d) = rep.drift.last()
                            && d.verdict != Verdict::Continue
                        {
                            decisions.push((
                                hash.clone(),
                                r.name.clone(),
                                r.stage,
                                d.verdict,
                                serde_json::to_value(d).unwrap_or_default(),
                            ));
                        }
                    }
                    Err(err) => {
                        sum.engine_failures += 1;
                        events.push(
                            Event::new(
                                t,
                                EventKind::EngineFailure,
                                json!({"strategy": r.name, "error": err.to_string()}),
                            )
                            .with_key(format!("engine:{hash}")),
                        );
                        failed.push(hash.clone());
                        break;
                    }
                }
            }
        }
        for h_ in failed {
            runners.remove(&h_);
        }
        for (hash, name, stage, verdict, d) in decisions {
            match verdict {
                Verdict::Demote => {
                    let to = reg
                        .demote(&hash, "sapma dedektörü (SPRT/CUSUM)", "sistem", t.0)
                        .map_err(|e| anyhow!("{e}"))?;
                    sum.demotions += 1;
                    events.push(Event::new(
                        t,
                        EventKind::Demotion,
                        json!({"strategy": name, "from": stage, "to": to, "drift": d}),
                    ));
                }
                Verdict::Promote if stage == Stage::Shadow => {
                    reg.set_stage(
                        &hash,
                        &name,
                        Stage::Paper,
                        "SPRT: gölge performansı gauntlet beklentisiyle tutarlı",
                        "sistem",
                        t.0,
                    )?;
                    sum.promotions += 1;
                    events.push(Event::new(
                        t,
                        EventKind::Promotion,
                        json!({"strategy": name, "from": stage, "to": "paper", "drift": d}),
                    ));
                }
                Verdict::Promote if stage.promotion_needs_approval() => {
                    let kind = if stage == Stage::Paper {
                        "micro_eval"
                    } else {
                        "scale"
                    };
                    let id = reg.request_approval(
                        kind,
                        &hash,
                        &json!({"strategy": name, "stage": stage, "evidence": d}),
                        t.0,
                    )?;
                    sum.approval_requests += 1;
                    events.push(
                        Event::new(
                            t,
                            EventKind::ApprovalRequest,
                            json!({"approval": id, "kind": kind, "strategy": name}),
                        )
                        .with_key(format!("approval:{id}")),
                    );
                }
                _ => {}
            }
        }
        // Onaylanmış talepler → terfi
        for a in reg.approvals()? {
            if let Some((d, _, _)) = &a.decision
                && d == "approved"
                && let Some(cur) = reg.current_stage(&a.subject)?
                // Onay tek kullanımlık ve mevcut aşamada açılmış olmalı (düşürme sonrası yeniden kullanılamaz).
                && !reg.approval_consumed(&a.subject, a.id)?
                && a.created_ts >= cur.ts
            {
                let to = match (a.kind.as_str(), cur.stage) {
                    ("micro_eval", Stage::Paper) => Some(Stage::MicroEval),
                    ("scale", Stage::MicroEval) => Some(Stage::Scale),
                    _ => None,
                };
                if let Some(to) = to {
                    reg.set_stage(
                        &a.subject,
                        &cur.name,
                        to,
                        &format!("onay #{} (Arman)", a.id),
                        "sistem",
                        t.0,
                    )?;
                    sum.promotions += 1;
                    events.push(Event::new(
                        t,
                        EventKind::Promotion,
                        json!({"strategy": cur.name, "to": to, "approval": a.id}),
                    ));
                }
            }
        }

        // 5. Arama
        if sum.ticks.is_multiple_of(search_every) {
            match searcher.cycle(&ctx, &short, &mut reg, t) {
                Ok((rep, reports)) => {
                    sum.screened += rep.screened as u64;
                    sum.gauntlet_runs += reports.len() as u64;
                    sum.gauntlet_passed += rep.gauntlet_passed.len() as u64;
                    for g in &reports {
                        events.push(Event::new(
                            t,
                            if g.passed { EventKind::GauntletPass } else { EventKind::ExperimentDone },
                            json!({"name": g.genome_name, "passed": g.passed, "failed_gate": g.failed_gate, "trades": g.trades_is}),
                        ));
                    }
                    events.push(Event::new(
                        t,
                        EventKind::SearchCycle,
                        serde_json::to_value(&rep)?,
                    ));
                }
                Err(e) => events.push(
                    Event::new(
                        t,
                        EventKind::EngineFailure,
                        json!({"component": "arama", "error": e.to_string()}),
                    )
                    .with_key("engine:search"),
                ),
            }
        }

        // 6. Ajanlar (yalnız sağlayıcı, fiyat ve bütçe yapılandırılmışsa)
        if !opts.disable_agents
            && cfg.agents.provider != "none"
            && t.since(last_agent) >= cfg.agents.research_every_minutes * NANOS_PER_MIN
        {
            last_agent = t;
            events.extend(crate::m3::agent_cycle(h, &cfg, &mut reg, t, &symbol));
        }

        // 7. Heartbeat, saat kayması
        if sum.ticks % cfg.daemon.heartbeat_minutes.max(1) == 1 || cfg.daemon.heartbeat_minutes <= 1
        {
            let (p, s, d) = sink.outbox.counts()?;
            events.push(Event::new(t, EventKind::Heartbeat, json!({"runners": runners.len(), "trials": reg.trial_count()?,
                "outbox": {"pending": p, "sent": s, "dead": d}, "bars": sum.bars, "replay": replay})));
        }
        if !replay
            && sum.ticks % 60 == 1
            && let Ok(api) = atlas_data::binance::BinanceFutures::new()
            && let Ok(server) = api.server_time()
            && let Some(e) =
                atlas_ops::clock_skew_event(now(), server, cfg.daemon.max_clock_skew_ms)
        {
            events.push(e);
        }

        // 8. Olay hunisi + teslim
        sum.events += events.len() as u64;
        sink.emit(&events, t)?;
        sink.maybe_digest(t)?;
        let d = sink.deliver(t, &sender)?;
        sum.delivered += d.sent;

        // 9. Checkpoint, kalıcılık
        if sum
            .ticks
            .is_multiple_of(cfg.daemon.checkpoint_every_minutes.max(1))
        {
            sink.journal.checkpoint(t)?;
        }
        if !replay
            && (sum
                .ticks
                .is_multiple_of(cfg.daemon.persist_every_minutes.max(1)))
            && !unsaved.is_empty()
        {
            store.append_bars(
                "binance_futures",
                &symbol,
                inst.tick_size,
                inst.lot_size,
                &unsaved,
                &atlas_core::calendar::format_utc(t),
            )?;
            unsaved.clear();
        }
        sd_notify("WATCHDOG=1");

        // 10. Durma koşulları
        if stop_file.exists() {
            let _ = std::fs::remove_file(&stop_file);
            sum.stopped_by = "durdurma dosyası".into();
            break;
        }
        if opts.max_ticks.is_some_and(|m| sum.ticks >= m) {
            sum.stopped_by = "tick sınırı".into();
            break;
        }
        if replay {
            clock = clock.plus(NANOS_PER_MIN);
            if feed.remaining() == Some(0) {
                // Son teslim denemeleri (geri çekilmede bekleyenler için sanal saat ilerletilir).
                for k in 1..=48u64 {
                    let tt = clock.plus(k * 30 * NANOS_PER_MIN);
                    sum.delivered += sink.deliver(tt, &sender)?.sent;
                }
                sink.journal.checkpoint(clock)?;
                sum.stopped_by = "replay bitti".into();
                break;
            }
        } else {
            let n = now();
            let next = UnixNanos(n.0 - n.0 % NANOS_PER_MIN + NANOS_PER_MIN + 5_000_000_000);
            std::thread::sleep(std::time::Duration::from_nanos(next.since(n)));
        }
    }
    if !unsaved.is_empty() {
        store.append_bars(
            "binance_futures",
            &symbol,
            inst.tick_size,
            inst.lot_size,
            &unsaved,
            &atlas_core::calendar::format_utc(now()),
        )?;
    }
    sum.outbox = sink.outbox.counts()?;
    Ok(sum)
}
