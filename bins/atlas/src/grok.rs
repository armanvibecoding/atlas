//! Grok operatör hesabı: CLI emirleri (`trade …`), daemon tarafı (ADR 0010) ve piyasa verisi (`market`).
//!
//! Akış: CLI `grok_order` olayı yazar (istek; seq = emir no) → daemon sonraki tick'te, o tick'in son
//! barında uygular ve `grok_order_applied {request_seq, bar_ts, order}` yazar → `ManualAccount` aynı
//! RiskGate/BarFill yoluyla işler. Yeniden başlatmada hesap depodaki barlar + `applied` kayıtlarıyla
//! birebir yeniden kurulur. Daemon her tick `var/grok_account.json` ve `var/market.json` yazar.

use crate::home::Home;
use crate::{Out, now, open_journal};
use anyhow::{Context as _, Result, anyhow, bail};
use atlas_core::units::{Fixed9, NANOS_PER_MIN};
use atlas_core::{Bar, Event, EventKind, Instrument, MarketEvent, Side, UnixNanos};
use atlas_exec::manual::{GROK_ID, ManualAccount, ManualOrder, QueuedOrder};
use atlas_journal::{EventFilter, Journal};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Piyasa anlık görüntüsünde tutulan bar sayısı (24 sa).
pub const MARKET_BARS: usize = 1440;

/// Dosyayı atomik yazar (geçici + yeniden adlandırma): okuyucu yarım dosya görmez.
pub fn write_atomic(path: &Path, v: &Value) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(v)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Journal'daki uygulanmış emirler (bar kapanışı → emirler) ve en büyük uygulanmış istek numarası.
pub fn load_applied(j: &Journal) -> Result<(BTreeMap<u64, Vec<QueuedOrder>>, i64)> {
    let mut map: BTreeMap<u64, Vec<QueuedOrder>> = BTreeMap::new();
    let mut last = 0i64;
    for e in j.read(&EventFilter {
        kind: Some(EventKind::GrokOrderApplied),
        ..Default::default()
    })? {
        let p = &e.event.payload;
        let (Some(seq), Some(bar), Some(order)) = (
            p.get("request_seq").and_then(Value::as_i64),
            p.get("bar_ts").and_then(Value::as_u64),
            p.get("order")
                .and_then(|o| serde_json::from_value::<ManualOrder>(o.clone()).ok()),
        ) else {
            continue;
        };
        last = last.max(seq);
        map.entry(bar).or_default().push(QueuedOrder {
            request_seq: seq,
            order,
        });
    }
    Ok((map, last))
}

/// `after` numarasından sonraki, henüz uygulanmamış emir istekleri.
pub fn new_requests(j: &Journal, after: i64) -> Result<Vec<QueuedOrder>> {
    let mut v = Vec::new();
    for e in j.read(&EventFilter {
        after_seq: Some(after),
        kind: Some(EventKind::GrokOrder),
        ..Default::default()
    })? {
        if let Some(order) = e
            .event
            .payload
            .get("order")
            .and_then(|o| serde_json::from_value::<ManualOrder>(o.clone()).ok())
        {
            v.push(QueuedOrder {
                request_seq: e.seq,
                order,
            });
        }
    }
    Ok(v)
}

/// Daemon tarafı: Grok hesabı + bekleyen uygulama kayıtları.
pub struct GrokDesk {
    pub account: ManualAccount,
    applied: BTreeMap<u64, Vec<QueuedOrder>>,
    last_request: i64,
}

impl GrokDesk {
    /// Journal'dan yeniden kurar. `stored` depodaki (kalıcı) barlar; uygulama kaydı olmayan hesap boş açılır.
    pub fn rebuild(
        j: &Journal,
        inst: &Instrument,
        gate: std::sync::Arc<atlas_risk::RiskGate>,
        start_if_new: UnixNanos,
        stored: impl Fn(UnixNanos) -> Result<Vec<Bar>>,
    ) -> Result<GrokDesk> {
        let (mut applied, last_request) = load_applied(j)?;
        let start = applied.keys().next().map_or(start_if_new, |first| {
            UnixNanos(first.saturating_sub(NANOS_PER_MIN))
        });
        let mut account = ManualAccount::new(inst, gate, start);
        if !applied.is_empty() {
            for b in stored(start)?.into_iter().filter(|b| b.close_ts > start) {
                let orders = applied.remove(&b.close_ts.0).unwrap_or_default();
                // Yeniden oynatma: olaylar zaten journal'da; tekrar yayılmaz. Hata yutulmaz.
                account
                    .step(&MarketEvent::bar(inst.id, b), orders)
                    .map_err(|e| anyhow!("grok hesabı yeniden kurulamadı: {e}"))?;
            }
        }
        Ok(GrokDesk {
            account,
            applied,
            last_request,
        })
    }

    /// Bu tick'in piyasa olaylarını işler. Yeni istekler son barda uygulanır ve kaydedilir.
    /// `wanted_kills`: CLI kill/kill-reset'ten türeyen ve bu hesaba düşen kill durumu; değiştiyse ilk
    /// uygun barda `SyncKills` olarak uygulanır ve kaydedilir (yeniden kurulum aynısını oynatır).
    pub fn step_tick(
        &mut self,
        j: &Journal,
        new_events: &[MarketEvent],
        t: UnixNanos,
        wanted_kills: &BTreeMap<String, String>,
    ) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        let first_bar = new_events.iter().position(|e| {
            matches!(e.payload, atlas_core::MarketPayload::Bar(_)) && e.ts >= self.account.last_ts()
        });
        // Yeni istekler bu tick'in son barında uygulanır — hesabın zaten geçtiği bir bara asla
        // (o bar işlenmez ve emir sessizce kaybolurdu). Uygun bar yoksa istekler sonraki tick'e kalır.
        let last_bar = new_events.iter().rposition(|e| {
            matches!(e.payload, atlas_core::MarketPayload::Bar(_)) && e.ts >= self.account.last_ts()
        });
        let reqs = if last_bar.is_some() {
            new_requests(j, self.last_request)?
        } else {
            Vec::new()
        };
        for (i, e) in new_events.iter().enumerate() {
            let mut orders = self.applied.remove(&e.ts.0).unwrap_or_default();
            // Yalnız kapsamlar karşılaştırılır: hesabın kendi kill'inin sebep metni journal'dakinden farklıdır
            // (strategy:grok öneki); metin karşılaştırılsaydı her tick yeni SyncKills yazılırdı.
            if Some(i) == first_bar && !wanted_kills.keys().eq(self.account.kills().keys()) {
                let o = QueuedOrder {
                    request_seq: 0,
                    order: ManualOrder::SyncKills {
                        kills: wanted_kills.clone(),
                    },
                };
                out.push(Event::new(
                    t,
                    EventKind::GrokOrderApplied,
                    json!({"request_seq": 0, "bar_ts": e.ts.0, "order": o.order, "account": GROK_ID}),
                ));
                orders.insert(0, o);
            }
            if Some(i) == last_bar {
                for r in &reqs {
                    out.push(Event::new(
                        t,
                        EventKind::GrokOrderApplied,
                        json!({"request_seq": r.request_seq, "bar_ts": e.ts.0, "order": r.order, "account": GROK_ID}),
                    ));
                    self.last_request = self.last_request.max(r.request_seq);
                }
                orders.extend(reqs.iter().cloned());
            }
            let evs = self
                .account
                .step(e, orders)
                .map_err(|err| anyhow!("grok hesabı: {err}"))?;
            out.extend(evs.into_iter().map(|mut ev| {
                if let Value::Object(m) = &mut ev.payload {
                    m.insert("account".into(), json!(GROK_ID));
                    // Grok hesabının kendi kill'i (günlük zarar, taban) yalnız bu hesabı kapsar: journal'da
                    // `account` kalsaydı kill senkronu sistemin bütün paper hesaplarını da durdururdu.
                    if ev.kind == EventKind::KillSwitch
                        && matches!(m.get("scope").and_then(Value::as_str), Some("account"))
                    {
                        m.insert("scope".into(), json!(format!("strategy:{GROK_ID}")));
                    }
                }
                ev
            }));
        }
        Ok(out)
    }
}

/// Piyasa anlık görüntüsü: son barlar (ondalık fiyat) — CLI `market` okur.
pub fn market_snapshot(inst: &Instrument, history: &[Bar], t: UnixNanos) -> Value {
    let px = |p: atlas_core::Price| inst.price_to_fixed9(p).as_f64();
    let from = history.len().saturating_sub(MARKET_BARS);
    let bars: Vec<Value> = history[from..]
        .iter()
        .map(|b| {
            json!([
                b.open_ts.0 / 1_000_000,
                px(b.open),
                px(b.high),
                px(b.low),
                px(b.close),
                Fixed9(
                    i64::try_from(b.volume)
                        .unwrap_or(i64::MAX)
                        .saturating_mul(inst.lot_size.0)
                )
                .as_f64()
            ])
        })
        .collect();
    let last_close = history
        .last()
        .map(|b| inst.price_to_fixed9(b.close).to_string());
    json!({"symbol": inst.symbol, "as_of": t, "last_close": last_close, "tick_size": inst.tick_size.to_string(),
        "lot_size": inst.lot_size.to_string(), "fields": ["open_time_ms","open","high","low","close","volume"], "bars": bars})
}

// ---------------------------------------------------------------- CLI

fn read_json(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{} yok — daemon çalışıyor mu?", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn instrument(h: &Home, symbol: &str) -> Result<Instrument> {
    Ok(h.instruments()?
        .get(symbol)
        .map_err(|e| anyhow!("{e}"))?
        .clone())
}

/// Ondalık fiyat (metin) → tick.
fn price(inst: &Instrument, s: &str, what: &str) -> Result<atlas_core::Price> {
    inst.parse_price(s.trim())
        .map_err(|e| anyhow!("{what} geçersiz ({s}): {e} — tick {}", inst.tick_size))
}

fn last_close(h: &Home) -> Result<Option<String>> {
    let m = read_json(&h.path("var/market.json"))?;
    Ok(m["last_close"].as_str().map(str::to_string))
}

fn clean_reason(words: &[String]) -> Result<String> {
    let r = words.join(" ");
    let r = crate::clean_input(&r.replace(['\n', '\r', '\t'], " "), 500)?;
    if r.chars().count() < 3 {
        bail!("gerekçe gerekli (en az 3 karakter)");
    }
    Ok(r)
}

fn append_order(h: &Home, symbol: &str, order: &ManualOrder) -> Result<i64> {
    let mut j = open_journal(h)?;
    Ok(j.append(&Event::new(
        now(),
        EventKind::GrokOrder,
        json!({"account": GROK_ID, "symbol": symbol, "order": order,
            "by": std::env::var("ATLAS_ROLE").unwrap_or_else(|_| "arman".into())}),
    ))?)
}

#[allow(clippy::too_many_arguments)]
pub fn trade_open(
    h: &Home,
    out: &Out,
    symbol: &str,
    side: &str,
    size: &str,
    stop: &str,
    target: Option<&str>,
    limit: Option<&str>,
    expiry_min: Option<u32>,
    reason: &[String],
) -> Result<bool> {
    let inst = instrument(h, symbol)?;
    let daemon_symbols = crate::daemon::m3_toml(h)?.daemon.symbols;
    if !daemon_symbols.iter().any(|s| s == symbol) {
        bail!("{symbol} canlı akışta yok (daemon sembolleri: {daemon_symbols:?})");
    }
    let side = match side.to_ascii_lowercase().as_str() {
        "buy" | "long" | "al" => Side::Buy,
        "sell" | "short" | "sat" => Side::Sell,
        s => bail!("side buy|sell olmalı ({s})"),
    };
    let sz = Fixed9::parse(size.trim()).map_err(|e| anyhow!("size geçersiz ({size}): {e}"))?;
    if sz.0 <= 0 || inst.lot_size.0 <= 0 || sz.0 % inst.lot_size.0 != 0 {
        bail!("size {} katı ve pozitif olmalı ({size})", inst.lot_size);
    }
    let qty = i32::try_from(sz.0 / inst.lot_size.0).map_err(|_| anyhow!("size çok büyük"))?;
    let stop_p = price(&inst, stop, "stop")?;
    let target_p = target.map(|t| price(&inst, t, "target")).transpose()?;
    let limit_p = limit.map(|l| price(&inst, l, "limit")).transpose()?;
    let reason = clean_reason(reason)?;
    // Referans fiyat: limit ya da son kapanış. Yön denetimi burada (erken, anlaşılır hata);
    // asıl risk denetimi daemon'da RiskGate'te.
    let reference = match limit_p {
        Some(p) => p,
        None => {
            let c = last_close(h)?.ok_or_else(|| anyhow!("son fiyat yok"))?;
            price(&inst, &c, "son fiyat")?
        }
    };
    let ok_stop = match side {
        Side::Buy => stop_p < reference,
        Side::Sell => stop_p > reference,
    };
    if !ok_stop {
        bail!(
            "stop yanlış tarafta: {} için stop {} referans fiyatın ({}) {} olmalı",
            if side == Side::Buy {
                "alış"
            } else {
                "satış"
            },
            stop.trim(),
            inst.price_to_fixed9(reference),
            if side == Side::Buy {
                "altında"
            } else {
                "üstünde"
            }
        );
    }
    if let Some(tp) = target_p {
        let ok = match side {
            Side::Buy => tp > reference,
            Side::Sell => tp < reference,
        };
        if !ok {
            bail!(
                "hedef yanlış tarafta (referans {})",
                inst.price_to_fixed9(reference)
            );
        }
    }
    // Risk kapısıyla aynı hesap: stop mesafesi + kayma tamponu + iki yön ücret.
    let policy = h.policy()?;
    let max = policy.max_risk_per_trade();
    let est_risk = atlas_risk::entry_risk(
        &inst,
        side,
        qty,
        reference,
        stop_p,
        policy.stop_slippage_buffer_ticks(),
    )
    .map_err(|e| anyhow!("risk hesaplanamadı: {e:?}"))?;
    if est_risk > max {
        // En büyük izinli adet: risk adetle doğrusal.
        let per_lot = atlas_risk::entry_risk(
            &inst,
            side,
            1,
            reference,
            stop_p,
            policy.stop_slippage_buffer_ticks(),
        )
        .map_err(|e| anyhow!("risk hesaplanamadı: {e:?}"))?;
        let max_qty = if per_lot.0 > 0 { max.0 / per_lot.0 } else { 0 };
        bail!(
            "risk çok yüksek: stopa kadar ~${:.2} (ücret dahil) > işlem başı tavan ${:.2}. En fazla size {} — boyutu küçült ya da stopu yaklaştır",
            est_risk.as_usd_f64(),
            max.as_usd_f64(),
            Fixed9(max_qty.saturating_mul(inst.lot_size.0))
        );
    }
    let order = ManualOrder::Open {
        side,
        qty,
        limit: limit_p,
        stop: stop_p,
        target: target_p,
        expiry_min: limit_p.map(|_| expiry_min.unwrap_or(60).clamp(1, 1440)),
        reason,
    };
    let seq = append_order(h, symbol, &order)?;
    out.emit(
        "trade.open",
        true,
        json!({"order_id": seq, "status": "queued", "qty": qty, "est_risk_usd": est_risk.as_usd_f64(),
            "note": "daemon sonraki dakikada uygular; dolum sonraki barın açılışından. Sonucu trade list ile gör."}),
        || format!("emir #{seq} kuyrukta (tahmini risk ${:.2})", est_risk.as_usd_f64()),
    );
    Ok(true)
}

pub fn trade_simple(
    h: &Home,
    out: &Out,
    symbol: &str,
    op: &str,
    reason: &[String],
) -> Result<bool> {
    let reason = clean_reason(reason)?;
    let order = match op {
        "close" => ManualOrder::Close { reason },
        "cancel" => ManualOrder::Cancel { reason },
        o => bail!("bilinmeyen işlem {o}"),
    };
    let seq = append_order(h, symbol, &order)?;
    out.emit(
        &format!("trade.{op}"),
        true,
        json!({"order_id": seq, "status": "queued"}),
        || format!("{op} emri #{seq} kuyrukta"),
    );
    Ok(true)
}

pub fn trade_list(h: &Home, out: &Out) -> Result<bool> {
    let mut snap = read_json(&h.path("var/grok_account.json"))?;
    // Henüz daemon'un almadığı istekler.
    let j = open_journal(h)?;
    let (_, last) = load_applied(&j)?;
    let queued: Vec<Value> = new_requests(&j, last)?
        .into_iter()
        .map(|q| json!({"order_id": q.request_seq, "order": q.order}))
        .collect();
    if let Value::Object(m) = &mut snap {
        m.insert("queued".into(), json!(queued));
        let age_s = now().since(UnixNanos(
            m.get("as_of").and_then(Value::as_u64).unwrap_or(0),
        )) / 1_000_000_000;
        m.insert("snapshot_age_s".into(), json!(age_s));
    }
    out.emit("trade.list", true, snap, || {
        "grok hesabı (--json ile ayrıntı)".into()
    });
    Ok(true)
}

/// Basit göstergeler (istatistik; f64 serbest).
fn ema(v: &[f64], n: usize) -> Option<f64> {
    if v.len() < n || n == 0 {
        return None;
    }
    let k = 2.0 / (n as f64 + 1.0);
    let mut e = v[..n].iter().sum::<f64>() / n as f64;
    for x in &v[n..] {
        e = x * k + e * (1.0 - k);
    }
    Some(e)
}

fn atr(bars: &[[f64; 6]], n: usize) -> Option<f64> {
    if bars.len() < n + 1 {
        return None;
    }
    let trs: Vec<f64> = bars
        .windows(2)
        .map(|w| {
            let (p, b) = (w[0], w[1]);
            (b[2] - b[3])
                .max((b[2] - p[4]).abs())
                .max((b[3] - p[4]).abs())
        })
        .collect();
    let tail = &trs[trs.len() - n..];
    Some(tail.iter().sum::<f64>() / n as f64)
}

fn aggregate(bars: &[[f64; 6]], minutes: u64) -> Vec<[f64; 6]> {
    let mut out: Vec<[f64; 6]> = Vec::new();
    let span = (minutes * 60_000) as f64;
    for b in bars {
        let bucket = (b[0] / span).floor() * span;
        match out.last_mut() {
            Some(l) if (l[0] - bucket).abs() < 1.0 => {
                l[2] = l[2].max(b[2]);
                l[3] = l[3].min(b[3]);
                l[4] = b[4];
                l[5] += b[5];
            }
            _ => out.push([bucket, b[1], b[2], b[3], b[4], b[5]]),
        }
    }
    out
}

fn summarize(tf: &str, bars: &[[f64; 6]], show: usize) -> Value {
    let closes: Vec<f64> = bars.iter().map(|b| b[4]).collect();
    let r = |x: Option<f64>| x.map(|v| (v * 100.0).round() / 100.0);
    let from = bars.len().saturating_sub(show);
    json!({
        "timeframe": tf,
        "ema20": r(ema(&closes, 20)),
        "ema50": r(ema(&closes, 50)),
        "atr14": r(atr(bars, 14)),
        "bars": bars[from..].iter().map(|b| json!({"t": b[0] as u64, "o": b[1], "h": b[2], "l": b[3], "c": b[4], "v": (b[5] * 1000.0).round() / 1000.0})).collect::<Vec<_>>(),
    })
}

pub fn market(h: &Home, out: &Out, bars_1m: usize) -> Result<bool> {
    let m = read_json(&h.path("var/market.json"))?;
    let bars: Vec<[f64; 6]> = m["bars"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| {
                    let v: Vec<f64> = b.as_array()?.iter().filter_map(Value::as_f64).collect();
                    (v.len() == 6).then(|| [v[0], v[1], v[2], v[3], v[4], v[5]])
                })
                .collect()
        })
        .unwrap_or_default();
    let Some(last) = bars.last() else {
        bail!("piyasa verisi boş");
    };
    let price_ago = |mins: usize| bars.iter().rev().nth(mins).map(|b| b[4]);
    let chg =
        |mins: usize| price_ago(mins).map(|p| ((last[4] / p - 1.0) * 10_000.0).round() / 100.0);
    let hi = bars.iter().map(|b| b[2]).fold(f64::MIN, f64::max);
    let lo = bars.iter().map(|b| b[3]).fold(f64::MAX, f64::min);
    // UTC gün VWAP'ı (tipik fiyat).
    let day = (last[0] / 86_400_000.0).floor() * 86_400_000.0;
    let (pv, vv) = bars
        .iter()
        .filter(|b| b[0] >= day)
        .fold((0.0, 0.0), |(pv, vv), b| {
            (pv + (b[2] + b[3] + b[4]) / 3.0 * b[5], vv + b[5])
        });
    let as_of = m["as_of"].as_u64().unwrap_or(0);
    let data = json!({
        "symbol": m["symbol"],
        "as_of": as_of,
        "age_s": now().since(UnixNanos(as_of)) / 1_000_000_000,
        "last": last[4],
        "change_pct": {"15m": chg(15), "1h": chg(60), "4h": chg(240), "24h": chg(bars.len().saturating_sub(1))},
        "range_24h": {"high": hi, "low": lo},
        "vwap_utc_day": if vv > 0.0 { Some(((pv / vv) * 100.0).round() / 100.0) } else { None },
        "tick_size": m["tick_size"],
        "lot_size": m["lot_size"],
        "m1": summarize("1m", &bars, bars_1m.clamp(1, 240)),
        "m15": summarize("15m", &aggregate(&bars, 15), 32),
        "h1": summarize("1h", &aggregate(&bars, 60), 24),
    });
    out.emit("market", true, data, || {
        format!("{} son {}", m["symbol"], last[4])
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::{InstrumentId, Price};
    use std::sync::Arc;

    /// Canlıda adım adım işlenen hesap ile journal + barlardan yeniden kurulan hesap birebir aynı olmalı.
    #[test]
    fn canli_ve_yeniden_kurulum_ayni() {
        let env = atlas_sim::validation::validation_env().unwrap();
        let inst = env.instruments.by_id(InstrumentId(0)).unwrap().clone();
        let gate = Arc::new(atlas_risk::RiskGate::new(env));
        let bars = atlas_sim::synthetic::ar_bars(5, 300, UnixNanos(0), 0.2, 20.0, Price(100_000));
        let mut j = Journal::open_in_memory().unwrap();
        let mut live =
            GrokDesk::rebuild(&j, &inst, Arc::clone(&gate), UnixNanos(0), |_| Ok(vec![])).unwrap();
        let req = |j: &mut Journal, o: ManualOrder| {
            j.append(&Event::new(
                UnixNanos(0),
                EventKind::GrokOrder,
                json!({"order": o}),
            ))
            .unwrap();
        };
        for (i, b) in bars.iter().enumerate() {
            if i == 20 || i == 110 || i == 150 {
                req(
                    &mut j,
                    ManualOrder::Open {
                        side: if i == 150 { Side::Sell } else { Side::Buy },
                        qty: 3,
                        limit: None,
                        stop: Price(if i == 150 {
                            b.close.0 + 60
                        } else {
                            b.close.0 - 60
                        }),
                        target: Some(Price(if i == 150 {
                            b.close.0 - 90
                        } else {
                            b.close.0 + 90
                        })),
                        expiry_min: None,
                        reason: "test".into(),
                    },
                );
            }
            if i == 120 || i == 260 {
                req(
                    &mut j,
                    ManualOrder::Close {
                        reason: "test kapat".into(),
                    },
                );
            }
            // 100–130 arası global kill (CLI kill → kill-reset) — yeniden kurulum aynı anı görmeli.
            let wanted: BTreeMap<String, String> = if (100..130).contains(&i) {
                [("global".to_string(), "test kill".to_string())].into()
            } else {
                BTreeMap::new()
            };
            let evs = live
                .step_tick(&j, &[MarketEvent::bar(inst.id, *b)], b.close_ts, &wanted)
                .unwrap();
            j.append_all(&evs).unwrap();
        }
        assert!(
            !live.account.trades().is_empty(),
            "en az bir işlem kapanmalı"
        );
        let rebuilt = GrokDesk::rebuild(&j, &inst, gate, UnixNanos(0), |from| {
            Ok(bars.iter().filter(|b| b.open_ts >= from).copied().collect())
        })
        .unwrap();
        assert_eq!(live.account.trades(), rebuilt.account.trades());
        assert_eq!(
            live.account.snapshot(UnixNanos(1))["position"],
            rebuilt.account.snapshot(UnixNanos(1))["position"]
        );
        assert_eq!(live.last_request, rebuilt.last_request);
        // Kill sırasında (i=110) verilen emir reddedilmiş olmalı; yeniden kurulumda da.
        let snap = rebuilt.account.snapshot(UnixNanos(1));
        let rejected = snap["orders"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|o| o["status"] == "rejected")
            .count();
        assert!(rejected >= 1, "{snap}");
        assert_eq!(
            snap["orders"],
            live.account.snapshot(UnixNanos(1))["orders"]
        );

        // Sebep metni farklı ama kapsam aynıysa yeni eşitleme kaydı yazılmaz (journal şişmez).
        let mut quiet = rebuilt;
        let same_scope: BTreeMap<String, String> =
            [("global".to_string(), "journal metni".to_string())].into();
        let b0 = bars[bars.len() - 1];
        let next = |k: u64| {
            MarketEvent::bar(
                inst.id,
                atlas_core::Bar {
                    open_ts: b0.close_ts.plus((k - 1) * NANOS_PER_MIN),
                    close_ts: b0.close_ts.plus(k * NANOS_PER_MIN),
                    ..b0
                },
            )
        };
        let syncs = |evs: &[Event]| {
            evs.iter()
                .filter(|e| e.kind == EventKind::GrokOrderApplied)
                .count()
        };
        let first = quiet
            .step_tick(&j, &[next(1)], b0.close_ts, &same_scope)
            .unwrap();
        assert_eq!(syncs(&first), 1, "kapsam değişti → bir kayıt");
        // Hesabın kendi kill kaydı farklı metinle (ör. günlük zarar) — kapsam aynı.
        quiet.account.apply_kills(&BTreeMap::new(), b0.close_ts);
        quiet.account.apply_kills(
            &[("global".to_string(), "hesabın kendi metni".to_string())].into(),
            b0.close_ts,
        );
        for k in 2..6 {
            let evs = quiet
                .step_tick(&j, &[next(k)], b0.close_ts, &same_scope)
                .unwrap();
            assert_eq!(syncs(&evs), 0, "aynı kapsam → yeni kayıt yok (tick {k})");
        }
    }
}
