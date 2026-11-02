//! Grok fiyat / seans alarmları (ADR 0012): Grok'u iki kontrol arasında webhook ile uyandırır.
//!
//! Akış (grok_order deseni): CLI `alert add` → journal `grok_alert_set` (seq = alarm no) → daemon her
//! barda etkin alarmları denetler, koşul sağlanınca `price_alert` (HIGH → sessiz saatte de webhook)
//! yazar. `alert cancel` → `grok_alert_cancelled`. Durum yalnız journal'dan kurulur: yeniden başlatmada
//! tetiklenmiş (journal'da `price_alert`'i olan) ya da iptal edilmiş alarm bir daha tetiklenmez.
//!
//! Kurallar: tek seferlik; fiyat alarmı en fazla 7 gün geçerli (varsayılan 24 sa); seans alarmı bir
//! sonraki hafta içi açılışta tetiklenir (açılıştan 60 dk sonra düşer); aynı anda en fazla 20 etkin
//! alarm. Alarm riski artırmaz: emir vermez, yalnız uyandırır.

use crate::home::Home;
use crate::{Out, now, open_journal};
use anyhow::{Result, anyhow, bail};
use atlas_core::calendar::{format_utc, local_date, local_to_utc, parse_tz};
use atlas_core::units::{NANOS_PER_HOUR, NANOS_PER_MIN};
use atlas_core::{Bar, Event, EventKind, Instrument, MarketEvent, MarketPayload, Price, UnixNanos};
use atlas_journal::{EventFilter, Journal};
use chrono::{Datelike, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Aynı anda en fazla etkin alarm.
pub const MAX_ACTIVE: usize = 20;
/// Fiyat alarmının en uzun geçerliliği (7 gün).
pub const MAX_EXPIRY_HOURS: u64 = 168;
pub const DEFAULT_EXPIRY_HOURS: u64 = 24;
/// Seans alarmı açılıştan bu kadar sonra (bar gelmediyse) düşer.
pub const SESSION_GRACE_MIN: u64 = 60;
const MAX_NOTE: usize = 200;

/// Seans açılışları: ad → (IANA saat dilimi, yerel saat:dakika). Yalnız hafta içi (yerel gün).
/// London: Arman'ın tanımıyla sabit 07:00 UTC. NY: US/Eastern 09:30, DST chrono-tz ile (core::calendar).
pub const SESSIONS: &[(&str, &str, u32)] = &[
    ("london", "UTC", 7 * 60),
    ("ny", "America/New_York", 9 * 60 + 30),
];

/// Alarm koşulu (journal `grok_alert_set` yükü).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertCond {
    /// Bar yükseği seviyeye değer ya da geçerse.
    PriceAbove { level: Price },
    /// Bar düşüğü seviyeye değer ya da altına inerse.
    PriceBelow { level: Price },
    /// Kapanışı açılış anına ulaşan ilk bar (açılış dakikası).
    SessionOpen { session: String, at: UnixNanos },
}

impl AlertCond {
    pub fn name(&self) -> &'static str {
        match self {
            AlertCond::PriceAbove { .. } => "price_above",
            AlertCond::PriceBelow { .. } => "price_below",
            AlertCond::SessionOpen { .. } => "session_open",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertSpec {
    pub symbol: String,
    pub cond: AlertCond,
    /// Kuruluş anı: kapanışı bundan sonra olan barlar denetlenir.
    pub created: UnixNanos,
    /// Kapanışı bundan sonra olan barlar tetiklemez.
    pub expires_at: UnixNanos,
    pub note: Option<String>,
}

/// Journal'dan kurulan alarm defteri.
#[derive(Debug, Default)]
pub struct AlertBook {
    alerts: BTreeMap<i64, AlertSpec>,
    cancelled: BTreeMap<i64, String>,
    /// alarm no → tetikleyen barın kapanışı.
    triggered: BTreeMap<i64, UnixNanos>,
    last_set: i64,
    last_cancel: i64,
}

fn read_kind(j: &Journal, kind: EventKind, after: i64) -> Result<Vec<atlas_journal::StoredEvent>> {
    Ok(j.read(&EventFilter {
        after_seq: Some(after),
        kind: Some(kind),
        ..Default::default()
    })?)
}

impl AlertBook {
    /// Journal'ın tamamından kurar (kuruluş, iptal, tetiklenme).
    pub fn load(j: &Journal) -> Result<AlertBook> {
        let mut b = AlertBook::default();
        b.refresh(j)?;
        for e in read_kind(j, EventKind::PriceAlert, 0)? {
            let p = &e.event.payload;
            if let (Some(id), Some(bar)) = (
                p.get("alert_id").and_then(Value::as_i64),
                p.get("bar_ts").and_then(Value::as_u64),
            ) {
                b.triggered.insert(id, UnixNanos(bar));
            }
        }
        Ok(b)
    }

    /// Son okumadan sonraki kuruluş ve iptal kayıtlarını alır. Türler ayrı sayaçla izlenir: iki okuma
    /// arasında yazılan kayıt kaçmaz; iptal, kuruluşundan önce okunsa da sonra eşleşir.
    pub fn refresh(&mut self, j: &Journal) -> Result<()> {
        for e in read_kind(j, EventKind::GrokAlertSet, self.last_set)? {
            self.last_set = self.last_set.max(e.seq);
            if let Some(spec) = e
                .event
                .payload
                .get("alert")
                .and_then(|a| serde_json::from_value::<AlertSpec>(a.clone()).ok())
            {
                self.alerts.insert(e.seq, spec);
            }
        }
        for e in read_kind(j, EventKind::GrokAlertCancelled, self.last_cancel)? {
            self.last_cancel = self.last_cancel.max(e.seq);
            let p = &e.event.payload;
            if let Some(id) = p.get("alert_id").and_then(Value::as_i64) {
                let reason = p
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.cancelled.entry(id).or_insert(reason);
            }
        }
        Ok(())
    }

    pub fn get(&self, id: i64) -> Option<&AlertSpec> {
        self.alerts.get(&id)
    }

    /// active | triggered | cancelled | expired (`at` anına göre).
    pub fn status(&self, id: i64, at: UnixNanos) -> &'static str {
        if self.triggered.contains_key(&id) {
            "triggered"
        } else if self.cancelled.contains_key(&id) {
            "cancelled"
        } else if self.alerts.get(&id).is_some_and(|s| at >= s.expires_at) {
            "expired"
        } else {
            "active"
        }
    }

    pub fn active_count(&self, at: UnixNanos) -> usize {
        self.alerts
            .keys()
            .filter(|id| self.status(**id, at) == "active")
            .count()
    }

    /// Bu barla tetiklenen alarmlar (yalnız `symbol`'deki ve bu bar için geçerli olanlar).
    fn hits(&self, symbol: &str, b: &Bar) -> Vec<i64> {
        self.alerts
            .iter()
            .filter(|(id, s)| {
                s.symbol == symbol
                    && !self.triggered.contains_key(id)
                    && !self.cancelled.contains_key(id)
                    && b.close_ts > s.created
                    && b.close_ts <= s.expires_at
                    && match &s.cond {
                        AlertCond::PriceAbove { level } => b.high >= *level,
                        AlertCond::PriceBelow { level } => b.low <= *level,
                        AlertCond::SessionOpen { at, .. } => b.close_ts >= *at,
                    }
            })
            .map(|(id, _)| *id)
            .collect()
    }

    /// Barları sırayla denetler; tetiklenenleri işaretler ve `price_alert` olaylarını döndürür.
    pub fn check(&mut self, inst: &Instrument, bars: &[MarketEvent], t: UnixNanos) -> Vec<Event> {
        let mut out = Vec::new();
        for e in bars {
            let MarketPayload::Bar(b) = &e.payload else {
                continue;
            };
            for id in self.hits(&inst.symbol, b) {
                self.triggered.insert(id, b.close_ts);
                if let Some(s) = self.alerts.get(&id) {
                    out.push(alert_event(inst, id, s, b, t));
                }
            }
        }
        out
    }

    /// Liste: etkinler + son 20 sonuçlanmış.
    fn listing(&self, inst_px: impl Fn(Price) -> String, at: UnixNanos) -> Value {
        let item = |id: i64, s: &AlertSpec| {
            let mut v = spec_json(&inst_px, id, s);
            v["status"] = json!(self.status(id, at));
            if let Some(b) = self.triggered.get(&id) {
                v["triggered_bar"] = json!(format_utc(*b));
            }
            if let Some(r) = self.cancelled.get(&id) {
                v["cancel_reason"] = json!(r);
            }
            v
        };
        let active: Vec<Value> = self
            .alerts
            .iter()
            .filter(|(id, _)| self.status(**id, at) == "active")
            .map(|(id, s)| item(*id, s))
            .collect();
        let recent: Vec<Value> = self
            .alerts
            .iter()
            .rev()
            .filter(|(id, _)| self.status(**id, at) != "active")
            .take(20)
            .map(|(id, s)| item(*id, s))
            .collect();
        json!({"active": active, "recent": recent,
            "limits": {"max_active": MAX_ACTIVE, "max_expiry_hours": MAX_EXPIRY_HOURS,
                "default_expiry_hours": DEFAULT_EXPIRY_HOURS, "session_grace_min": SESSION_GRACE_MIN}})
    }
}

/// Okunur alarm özeti (CLI ve olay yükleri).
fn spec_json(px: &impl Fn(Price) -> String, id: i64, s: &AlertSpec) -> Value {
    let mut v = json!({"alert_id": id, "symbol": s.symbol, "condition": s.cond.name(),
        "created": format_utc(s.created), "expires": format_utc(s.expires_at), "note": s.note});
    match &s.cond {
        AlertCond::PriceAbove { level } | AlertCond::PriceBelow { level } => {
            v["level"] = json!(px(*level));
        }
        AlertCond::SessionOpen { session, at } => {
            v["session"] = json!(session);
            v["session_open"] = json!(format_utc(*at));
        }
    }
    v
}

fn alert_event(inst: &Instrument, id: i64, s: &AlertSpec, b: &Bar, t: UnixNanos) -> Event {
    let px = |p: Price| inst.price_to_fixed9(p).to_string();
    let message = match &s.cond {
        AlertCond::PriceAbove { level } => format!(
            "{} {} seviyesine çıktı (bar yükseği {}, kapanış {})",
            s.symbol,
            px(*level),
            px(b.high),
            px(b.close)
        ),
        AlertCond::PriceBelow { level } => format!(
            "{} {} seviyesine indi (bar düşüğü {}, kapanış {})",
            s.symbol,
            px(*level),
            px(b.low),
            px(b.close)
        ),
        AlertCond::SessionOpen { session, at } => format!(
            "{} seans açılışı ({}); {} son {}",
            session,
            format_utc(*at),
            s.symbol,
            px(b.close)
        ),
    };
    let mut p = spec_json(&px, id, s);
    p["bar_ts"] = json!(b.close_ts.0);
    p["bar_time"] = json!(format_utc(b.close_ts));
    p["bar"] =
        json!({"open": px(b.open), "high": px(b.high), "low": px(b.low), "close": px(b.close)});
    p["message"] = json!(message);
    p["account"] = json!(atlas_exec::manual::GROK_ID);
    Event::new(t, EventKind::PriceAlert, p).with_key(format!("price_alert:{id}"))
}

/// `session`'ın `after`'dan sonraki ilk hafta içi açılışı (UTC). Saat dilimi/DST `core::calendar`.
pub fn next_session_open(session: &str, after: UnixNanos) -> Result<UnixNanos> {
    let (_, tz, minute) = SESSIONS
        .iter()
        .find(|(n, _, _)| *n == session)
        .ok_or_else(|| {
            anyhow!(
                "bilinmeyen seans {session} (şunlardan biri: {})",
                SESSIONS.iter().map(|s| s.0).collect::<Vec<_>>().join(", ")
            )
        })?;
    let tz = parse_tz(tz).map_err(|e| anyhow!("{e}"))?;
    let today = local_date(after, tz);
    for d in 0..10 {
        let Some(day) = today.checked_add_days(chrono::Days::new(d)) else {
            break;
        };
        if matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            continue;
        }
        let at = local_to_utc(day, *minute, tz);
        if at > after {
            return Ok(at);
        }
    }
    bail!("{session}: sonraki açılış bulunamadı")
}

/// Koşul argümanları (CLI bayrakları).
pub struct CondArgs<'a> {
    pub above: Option<&'a str>,
    pub below: Option<&'a str>,
    pub session: Option<&'a str>,
}

/// Alarm tanımını doğrular ve kurar. `last` son kapanış (fiyat alarmında yön denetimi için zorunlu).
pub fn build_spec(
    inst: &Instrument,
    cond: &CondArgs<'_>,
    last: Option<Price>,
    at: UnixNanos,
    expiry_hours: Option<u64>,
    note: Option<String>,
) -> Result<AlertSpec> {
    let price = |s: &str| {
        inst.parse_price(s.trim())
            .map_err(|e| anyhow!("fiyat geçersiz ({s}): {e} — tick {}", inst.tick_size))
    };
    let need_last = || last.ok_or_else(|| anyhow!("son fiyat yok — daemon çalışıyor mu?"));
    let px = |p: Price| inst.price_to_fixed9(p);
    let (c, expires_at) = match (cond.above, cond.below, cond.session) {
        (Some(a), None, None) | (None, Some(a), None) => {
            let level = price(a)?;
            let l = need_last()?;
            let c = if cond.above.is_some() {
                if level <= l {
                    bail!(
                        "fiyat zaten {} üstünde (son {}); --above son fiyatın üstünde olmalı",
                        px(level),
                        px(l)
                    );
                }
                AlertCond::PriceAbove { level }
            } else {
                if level >= l {
                    bail!(
                        "fiyat zaten {} altında (son {}); --below son fiyatın altında olmalı",
                        px(level),
                        px(l)
                    );
                }
                AlertCond::PriceBelow { level }
            };
            let hours = expiry_hours.unwrap_or(DEFAULT_EXPIRY_HOURS);
            if !(1..=MAX_EXPIRY_HOURS).contains(&hours) {
                bail!("--expiry-hours 1..{MAX_EXPIRY_HOURS} olmalı ({hours})");
            }
            (c, at.plus(hours * NANOS_PER_HOUR))
        }
        (None, None, Some(s)) => {
            if expiry_hours.is_some() {
                bail!(
                    "seans alarmında --expiry-hours verilmez (açılıştan {SESSION_GRACE_MIN} dk sonra düşer)"
                );
            }
            let open = next_session_open(s, at)?;
            (
                AlertCond::SessionOpen {
                    session: s.to_string(),
                    at: open,
                },
                open.plus(SESSION_GRACE_MIN * NANOS_PER_MIN),
            )
        }
        _ => bail!(
            "tam olarak biri gerekli: --above <fiyat> | --below <fiyat> | --session london|ny"
        ),
    };
    Ok(AlertSpec {
        symbol: inst.symbol.clone(),
        cond: c,
        created: at,
        expires_at,
        note,
    })
}

// ---------------------------------------------------------------- daemon

/// Daemon tarafı: alarm defteri + izlenen enstrüman.
pub struct AlertDesk {
    book: AlertBook,
    inst: Instrument,
}

impl AlertDesk {
    /// Journal'dan kurar (yeniden başlatmada tetiklenmiş/iptal alarmlar etkin sayılmaz).
    pub fn rebuild(j: &Journal, inst: &Instrument) -> Result<AlertDesk> {
        Ok(AlertDesk {
            book: AlertBook::load(j)?,
            inst: inst.clone(),
        })
    }

    /// Yeni kayıtları alır, bu tick'in barlarını denetler. Hata yutulmaz: `engine_failure` olur.
    pub fn step_tick(&mut self, j: &Journal, new_bars: &[MarketEvent], t: UnixNanos) -> Vec<Event> {
        if let Err(e) = self.book.refresh(j) {
            return vec![
                Event::new(
                    t,
                    EventKind::EngineFailure,
                    json!({"component": "grok_alarmları", "error": e.to_string()}),
                )
                .with_key("engine:alerts"),
            ];
        }
        self.book.check(&self.inst, new_bars, t)
    }
}

// ---------------------------------------------------------------- CLI

/// `atlas alert …` (Grok'a izinli; riski artırmaz).
#[derive(clap::Subcommand)]
pub enum AlertCmd {
    /// Alarm kur — tam olarak biri: --above <fiyat> | --below <fiyat> | --session london|ny.
    /// Tek seferlik; tetiklenince webhook (price_alert, HIGH) Grok'u uyandırır.
    Add {
        /// Fiyat bu seviyeye çıkarsa (son fiyatın üstünde olmalı).
        #[arg(long)]
        above: Option<String>,
        /// Fiyat bu seviyeye inerse (son fiyatın altında olmalı).
        #[arg(long)]
        below: Option<String>,
        /// Sonraki hafta içi seans açılışı: london (07:00 UTC) | ny (09:30 US/Eastern, DST dahil).
        #[arg(long)]
        session: Option<String>,
        /// Fiyat alarmının geçerliliği (saat, 1..168, varsayılan 24). Seans alarmında verilmez.
        #[arg(long)]
        expiry_hours: Option<u64>,
        #[arg(long, default_value = "BTCUSDT")]
        symbol: String,
        /// Not (isteğe bağlı, ≤200 karakter; tetiklenince olayda geri gelir).
        note: Vec<String>,
    },
    /// Etkin alarmlar + son sonuçlananlar (triggered / cancelled / expired).
    List,
    /// Etkin alarmı iptal et.
    Cancel { id: i64, reason: Vec<String> },
}

pub fn run(h: &Home, out: &Out, cmd: AlertCmd) -> Result<bool> {
    match cmd {
        AlertCmd::Add {
            above,
            below,
            session,
            expiry_hours,
            symbol,
            note,
        } => add(
            h,
            out,
            &symbol,
            &CondArgs {
                above: above.as_deref(),
                below: below.as_deref(),
                session: session.as_deref(),
            },
            expiry_hours,
            &note,
        ),
        AlertCmd::List => list(h, out),
        AlertCmd::Cancel { id, reason } => cancel(h, out, id, &reason),
    }
}

fn daemon_instrument(h: &Home, symbol: &str) -> Result<Instrument> {
    let live = crate::daemon::m3_toml(h)?.daemon.symbols;
    if live.first().map(String::as_str) != Some(symbol) {
        bail!("{symbol} alarm için izlenmiyor (daemon sembolü: {live:?})");
    }
    Ok(h.instruments()?
        .get(symbol)
        .map_err(|e| anyhow!("{e}"))?
        .clone())
}

/// `var/market.json`'daki son kapanış (yoksa `None`).
fn last_close(h: &Home, inst: &Instrument) -> Option<Price> {
    let text = std::fs::read_to_string(h.path("var/market.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    inst.parse_price(v.get("last_close")?.as_str()?).ok()
}

fn role() -> String {
    std::env::var("ATLAS_ROLE").unwrap_or_else(|_| "arman".into())
}

fn clean_text(words: &[String], max: usize) -> Result<Option<String>> {
    let t = crate::clean_input(&words.join(" ").replace(['\n', '\r', '\t'], " "), max)?;
    Ok((!t.is_empty()).then_some(t))
}

pub fn add(
    h: &Home,
    out: &Out,
    symbol: &str,
    cond: &CondArgs<'_>,
    expiry_hours: Option<u64>,
    note: &[String],
) -> Result<bool> {
    let inst = daemon_instrument(h, symbol)?;
    let note = clean_text(note, MAX_NOTE)?;
    let t = now();
    let spec = build_spec(&inst, cond, last_close(h, &inst), t, expiry_hours, note)?;
    let mut j = open_journal(h)?;
    let book = AlertBook::load(&j)?;
    let n = book.active_count(t);
    if n >= MAX_ACTIVE {
        bail!("en fazla {MAX_ACTIVE} etkin alarm (şu an {n}); önce alert cancel <id>");
    }
    let px = |p: Price| inst.price_to_fixed9(p).to_string();
    let mut payload = spec_json(&px, 0, &spec);
    if let Value::Object(m) = &mut payload {
        m.remove("alert_id");
        m.insert("alert".into(), serde_json::to_value(&spec)?);
        m.insert("by".into(), json!(role()));
    }
    let seq = j.append(&Event::new(t, EventKind::GrokAlertSet, payload))?;
    let view = spec_json(&px, seq, &spec);
    out.emit(
        "alert.add",
        true,
        json!({"alert_id": seq, "status": "active", "alert": view, "active_count": n + 1}),
        || {
            format!(
                "alarm #{seq} kuruldu ({}), bitiş {}",
                spec.cond.name(),
                format_utc(spec.expires_at)
            )
        },
    );
    Ok(true)
}

pub fn list(h: &Home, out: &Out) -> Result<bool> {
    let j = open_journal(h)?;
    let book = AlertBook::load(&j)?;
    let insts = h.instruments()?;
    let px = |p: Price| {
        // Defterde tek sembol beklenir (daemon sembolü); tick bilinmiyorsa ham tick yazılır.
        crate::daemon::m3_toml(h)
            .ok()
            .and_then(|c| c.daemon.symbols.first().cloned())
            .and_then(|s| insts.get(&s).ok().map(|i| i.price_to_fixed9(p).to_string()))
            .unwrap_or_else(|| format!("{}t", p.0))
    };
    let t = now();
    let data = book.listing(px, t);
    out.emit("alert.list", true, data.clone(), || {
        let a = data["active"].as_array().map_or(0, Vec::len);
        format!("{a} etkin alarm (--json ile ayrıntı)")
    });
    Ok(true)
}

pub fn cancel(h: &Home, out: &Out, id: i64, reason: &[String]) -> Result<bool> {
    let reason = clean_text(reason, 500)?.unwrap_or_else(|| "gerekçesiz".into());
    let mut j = open_journal(h)?;
    let book = AlertBook::load(&j)?;
    if book.get(id).is_none() {
        bail!("alarm #{id} yok");
    }
    let st = book.status(id, now());
    if st != "active" {
        bail!("alarm #{id} etkin değil ({st})");
    }
    let seq = j.append(&Event::new(
        now(),
        EventKind::GrokAlertCancelled,
        json!({"alert_id": id, "reason": reason, "by": role()}),
    ))?;
    out.emit(
        "alert.cancel",
        true,
        json!({"alert_id": id, "status": "cancelled", "seq": seq}),
        || format!("alarm #{id} iptal edildi"),
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::calendar::parse_rfc3339;
    use atlas_core::{InstrumentId, InstrumentTable};

    fn ts(s: &str) -> UnixNanos {
        parse_rfc3339(s).unwrap()
    }

    fn btc() -> Instrument {
        InstrumentTable::from_toml_str(
            "[[instrument]]\nsymbol = \"BTCUSDT\"\nvenue = \"binance\"\nkind = \"perpetual\"\n\
             tick_size = \"0.1\"\ntick_value_usd = \"0.0001\"\nlot_size = \"0.001\"\nsession = \"always\"\n\
             [instrument.costs]\nfee_rate_percent = \"0.05\"\nslippage_ticks = \"1\"\n",
        )
        .unwrap()
        .get("BTCUSDT")
        .unwrap()
        .clone()
    }

    /// `start`'tan itibaren 1m barlar: (yüksek, düşük, kapanış) tick cinsinden.
    fn bars(start: UnixNanos, hlc: &[(i64, i64, i64)]) -> Vec<MarketEvent> {
        hlc.iter()
            .enumerate()
            .map(|(i, (h, l, c))| {
                let open_ts = start.plus(i as u64 * NANOS_PER_MIN);
                MarketEvent::bar(
                    InstrumentId(0),
                    Bar {
                        open_ts,
                        close_ts: open_ts.plus(NANOS_PER_MIN),
                        open: Price(*c),
                        high: Price(*h),
                        low: Price(*l),
                        close: Price(*c),
                        volume: 1,
                        buy_volume: 0,
                        trades: 1,
                    },
                )
            })
            .collect()
    }

    fn set(j: &mut Journal, spec: &AlertSpec) -> i64 {
        j.append(&Event::new(
            spec.created,
            EventKind::GrokAlertSet,
            json!({"alert": spec}),
        ))
        .unwrap()
    }

    fn cond<'a>(
        above: Option<&'a str>,
        below: Option<&'a str>,
        session: Option<&'a str>,
    ) -> CondArgs<'a> {
        CondArgs {
            above,
            below,
            session,
        }
    }

    #[test]
    fn seans_acilisi_dst_ve_hafta_sonu() {
        // 2026-11-02 Pazartesi: ABD kış saati (EST, UTC−5) → NY 09:30 = 14:30Z.
        let mon = ts("2026-11-02T10:00:00Z");
        assert_eq!(
            next_session_open("ny", mon).unwrap(),
            ts("2026-11-02T14:30:00Z")
        );
        // London sabit 07:00 UTC; o gün geçti → ertesi gün.
        assert_eq!(
            next_session_open("london", mon).unwrap(),
            ts("2026-11-03T07:00:00Z")
        );
        // Yaz saati (EDT, UTC−4): 2026-07-01 Çarşamba → 13:30Z.
        assert_eq!(
            next_session_open("ny", ts("2026-07-01T12:00:00Z")).unwrap(),
            ts("2026-07-01T13:30:00Z")
        );
        // Cuma açılıştan sonra → Pazartesi; Cumartesi → Pazartesi.
        assert_eq!(
            next_session_open("ny", ts("2026-11-06T15:00:00Z")).unwrap(),
            ts("2026-11-09T14:30:00Z")
        );
        assert_eq!(
            next_session_open("london", ts("2026-11-07T12:00:00Z")).unwrap(),
            ts("2026-11-09T07:00:00Z")
        );
        // DST geçiş haftası: 2026-03-09 Pazartesi (ABD 03-08'de yaz saatine geçti) → 13:30Z.
        assert_eq!(
            next_session_open("ny", ts("2026-03-09T00:00:00Z")).unwrap(),
            ts("2026-03-09T13:30:00Z")
        );
        assert!(next_session_open("tokyo", mon).is_err());
    }

    #[test]
    fn tanim_dogrulamasi() {
        let i = btc();
        let t = ts("2026-11-02T10:00:00Z");
        let last = Some(Price(780_000));
        let s = build_spec(&i, &cond(Some("79000"), None, None), last, t, None, None).unwrap();
        assert_eq!(
            s.cond,
            AlertCond::PriceAbove {
                level: Price(790_000)
            }
        );
        assert_eq!(s.expires_at, t.plus(24 * NANOS_PER_HOUR));
        // Fiyat zaten üstünde / altında → ret; son fiyat yok → ret.
        assert!(build_spec(&i, &cond(Some("77000"), None, None), last, t, None, None).is_err());
        assert!(build_spec(&i, &cond(None, Some("79000"), None), last, t, None, None).is_err());
        assert!(build_spec(&i, &cond(Some("79000"), None, None), None, t, None, None).is_err());
        // Süre 1..168 sa.
        assert!(
            build_spec(
                &i,
                &cond(None, Some("77000"), None),
                last,
                t,
                Some(169),
                None
            )
            .is_err()
        );
        assert!(build_spec(&i, &cond(None, Some("77000"), None), last, t, Some(0), None).is_err());
        let s = build_spec(
            &i,
            &cond(None, Some("77000"), None),
            last,
            t,
            Some(168),
            None,
        )
        .unwrap();
        assert_eq!(s.expires_at, t.plus(7 * 24 * NANOS_PER_HOUR));
        // Tam olarak bir koşul; tick dışı fiyat ret.
        assert!(
            build_spec(
                &i,
                &cond(Some("79000"), Some("77000"), None),
                last,
                t,
                None,
                None
            )
            .is_err()
        );
        assert!(build_spec(&i, &cond(None, None, None), last, t, None, None).is_err());
        assert!(build_spec(&i, &cond(Some("79000.05"), None, None), last, t, None, None).is_err());
        // Seans: son fiyat gerekmez; süre verilmez; açılış + 60 dk'da düşer.
        let s = build_spec(&i, &cond(None, None, Some("ny")), None, t, None, None).unwrap();
        let open = ts("2026-11-02T14:30:00Z");
        assert_eq!(
            s.cond,
            AlertCond::SessionOpen {
                session: "ny".into(),
                at: open
            }
        );
        assert_eq!(s.expires_at, open.plus(60 * NANOS_PER_MIN));
        assert!(build_spec(&i, &cond(None, None, Some("ny")), None, t, Some(5), None).is_err());
    }

    #[test]
    fn fiyat_alarmi_bir_kez_tetiklenir_ve_yeniden_kurulumda_tekrar_etmez() {
        let i = btc();
        let t0 = ts("2026-11-02T10:00:30Z");
        let mut j = Journal::open_in_memory().unwrap();
        let spec = build_spec(
            &i,
            &cond(Some("79000"), None, None),
            Some(Price(780_000)),
            t0,
            None,
            Some("direnç".into()),
        )
        .unwrap();
        let id = set(&mut j, &spec);
        let mut desk = AlertDesk::rebuild(&j, &i).unwrap();
        // Kuruluştan önce kapanan bar (10:00) yükseği seviyeyi geçse de sayılmaz.
        let start = ts("2026-11-02T09:59:00Z");
        let b = bars(
            start,
            &[
                (791_000, 779_000, 780_000), // 09:59–10:00: kuruluştan önce
                (785_000, 779_000, 784_000), // altında
                (790_000, 783_000, 789_000), // yükseği tam seviye → tetik
                (795_000, 788_000, 794_000), // tekrar yok
            ],
        );
        let evs = desk.step_tick(&j, &b, ts("2026-11-02T10:03:05Z"));
        assert_eq!(evs.len(), 1, "{evs:?}");
        assert_eq!(evs[0].kind, EventKind::PriceAlert);
        assert_eq!(evs[0].priority, atlas_core::Priority::High);
        assert_eq!(
            evs[0].key.as_deref(),
            Some(format!("price_alert:{id}").as_str())
        );
        assert_eq!(evs[0].payload["level"], "79000");
        assert_eq!(evs[0].payload["note"], "direnç");
        assert_eq!(evs[0].payload["bar_time"], "2026-11-02T10:02:00Z");
        j.append_all(&evs).unwrap();
        // Aynı barlar ve sonrası tekrar gelse de (yeniden başlatma + boşluk telafisi) tetiklenmez.
        let mut again = AlertDesk::rebuild(&j, &i).unwrap();
        assert!(
            again
                .step_tick(&j, &b, ts("2026-11-02T10:05:00Z"))
                .is_empty()
        );
        assert_eq!(again.book.status(id, t0), "triggered");
        assert_eq!(again.book.active_count(t0), 0);
    }

    #[test]
    fn iptal_suresi_dolmus_ve_seans_alarmi() {
        let i = btc();
        let t0 = ts("2026-11-02T10:00:00Z");
        let last = Some(Price(780_000));
        let mut j = Journal::open_in_memory().unwrap();
        let below = build_spec(
            &i,
            &cond(None, Some("77000"), None),
            last,
            t0,
            Some(1),
            None,
        )
        .unwrap();
        let cancelled =
            build_spec(&i, &cond(None, Some("77500"), None), last, t0, None, None).unwrap();
        let session = build_spec(&i, &cond(None, None, Some("ny")), None, t0, None, None).unwrap();
        let id_below = set(&mut j, &below);
        let id_cancel = set(&mut j, &cancelled);
        let id_session = set(&mut j, &session);
        let mut desk = AlertDesk::rebuild(&j, &i).unwrap();
        // İptal, daemon kurulduktan sonra yazılır → refresh ile alınır.
        j.append(&Event::new(
            t0,
            EventKind::GrokAlertCancelled,
            json!({"alert_id": id_cancel, "reason": "vazgeçtim"}),
        ))
        .unwrap();
        // 11:30'da 77000'in altına iniş: "below" 1 sa sonra (11:00) düşmüştü; iptal edilen de tetiklenmez.
        let late = bars(ts("2026-11-02T11:30:00Z"), &[(780_000, 760_000, 765_000)]);
        assert!(
            desk.step_tick(&j, &late, ts("2026-11-02T11:31:05Z"))
                .is_empty()
        );
        assert_eq!(
            desk.book.status(id_below, ts("2026-11-02T11:31:00Z")),
            "expired"
        );
        assert_eq!(desk.book.status(id_cancel, t0), "cancelled");
        // Seans: kapanışı 14:30'a ulaşan ilk bar (14:29–14:30) tetikler.
        let around = bars(
            ts("2026-11-02T14:28:00Z"),
            &[
                (780_000, 779_000, 779_500),
                (780_000, 779_000, 779_600),
                (780_000, 779_000, 779_700),
            ],
        );
        let evs = desk.step_tick(&j, &around, ts("2026-11-02T14:31:05Z"));
        assert_eq!(evs.len(), 1, "{evs:?}");
        assert_eq!(evs[0].payload["alert_id"], id_session);
        assert_eq!(evs[0].payload["bar_time"], "2026-11-02T14:30:00Z");
        assert_eq!(evs[0].payload["session_open"], "2026-11-02T14:30:00Z");
        // Liste: etkin yok; sonuçlanmışlar durumlarıyla.
        j.append_all(&evs).unwrap();
        let book = AlertBook::load(&j).unwrap();
        let l = book.listing(
            |p| i.price_to_fixed9(p).to_string(),
            ts("2026-11-02T15:00:00Z"),
        );
        assert_eq!(l["active"].as_array().unwrap().len(), 0);
        let st: Vec<&str> = l["recent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["status"].as_str().unwrap())
            .collect();
        assert_eq!(st, ["triggered", "cancelled", "expired"]);
    }

    #[test]
    fn etkin_alarm_sayisi() {
        let i = btc();
        let t0 = ts("2026-11-02T10:00:00Z");
        let mut j = Journal::open_in_memory().unwrap();
        for k in 0..MAX_ACTIVE {
            let s = build_spec(
                &i,
                &cond(Some(&format!("{}", 79_000 + k)), None, None),
                Some(Price(780_000)),
                t0,
                None,
                None,
            )
            .unwrap();
            set(&mut j, &s);
        }
        let book = AlertBook::load(&j).unwrap();
        assert_eq!(book.active_count(t0), MAX_ACTIVE);
        // 24 sa sonra hepsi düşmüş sayılır.
        assert_eq!(book.active_count(t0.plus(24 * NANOS_PER_HOUR)), 0);
    }
}
