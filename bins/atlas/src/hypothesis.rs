//! `atlas hypothesis submit|list`: operatörün (Grok) yazılı strateji hipotezi kanalı.
//!
//! - Hipotez yapılandırılmıştır: fikir, kaynak, zaman dilimi, giriş kuralları (genom koşul DSL'i),
//!   isteğe bağlı çıkış/seans/çürütme ölçütü. Şemaya uymayan alan reddedilir (kural 9'un ruhu).
//! - **Parametreyi sistem seçer:** hipotezin sayıları yalnız başlangıç noktasıdır. Temel genom ve
//!   her parametrenin ±%25 komşuları birlikte arama kuyruğuna girer; hepsi normal aday gibi taramadan
//!   ve gauntlet'ten geçer, **her biri deneme sayılır** (DSR, kural 7). Kuyruk aramadan önce işlenir.
//! - Deneme sayacını şişirmemek için günde en fazla [`DAILY_LIMIT`] hipotez; aynı hipotez tekrar girmez.

use crate::Out;
use crate::home::Home;
use anyhow::{Result, anyhow, bail};
use atlas_core::UnixNanos;
use atlas_core::units::NANOS_PER_DAY;
use atlas_genome::{Entry, Exit, Filters, Genome, Sizing, TIMEFRAMES};
use atlas_registry::Registry;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SOURCE: &str = "operatör:grok";
pub const DAILY_LIMIT: usize = 5;
/// Temel genom + en fazla bu kadar parametre komşusu.
pub const MAX_VARIANTS: usize = 16;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hypothesis {
    /// Fikir (ne ve neden çalışmalı).
    pub idea: String,
    /// Fikrin kaynağı (gözlem, kitap, makale, bağlantı …).
    pub source: String,
    /// "1m" | "5m" | "15m" | "30m" | "1h" | "4h"
    pub timeframe: String,
    /// Giriş kuralları: genom koşul DSL'i (`long` ve/veya `short`).
    pub entry: Entry,
    /// Çıkış: boşsa ATR(14) × 1,5 stop, 2R hedef.
    #[serde(default)]
    pub exit: Option<Exit>,
    /// UTC saat penceresi [başlangıç, bitiş).
    #[serde(default)]
    pub session_utc: Option<(u32, u32)>,
    /// Hipotezi neyin çürüteceği.
    #[serde(default)]
    pub falsification: Option<String>,
}

pub fn timeframe_minutes(tf: &str) -> Option<u32> {
    let m = match tf {
        "1m" => 1,
        "5m" => 5,
        "15m" => 15,
        "30m" => 30,
        "1h" => 60,
        "4h" => 240,
        _ => return None,
    };
    TIMEFRAMES.contains(&m).then_some(m)
}

fn check_text(name: &str, v: &str, min: usize, max: usize) -> Result<()> {
    let n = v.trim().chars().count();
    if n < min || n > max {
        bail!("'{name}' {min}..{max} karakter olmalı");
    }
    if v.chars().any(char::is_control) {
        bail!("'{name}' kontrol karakteri içeriyor");
    }
    Ok(())
}

impl Hypothesis {
    pub fn parse(text: &str) -> Result<Hypothesis> {
        let h: Hypothesis =
            serde_json::from_str(text).map_err(|e| anyhow!("hipotez şemaya uymuyor: {e}"))?;
        check_text("idea", &h.idea, 10, 1000)?;
        check_text("source", &h.source, 3, 300)?;
        if let Some(f) = &h.falsification {
            check_text("falsification", f, 3, 500)?;
        }
        Ok(h)
    }

    /// Temel genom (doğrulanmış). Ad ve etiketler hash'e girmez.
    pub fn genome(&self, instrument: &str, risk_usd: u32) -> Result<Genome> {
        let tf = timeframe_minutes(&self.timeframe)
            .ok_or_else(|| anyhow!("timeframe: 1m|5m|15m|30m|1h|4h"))?;
        let g = Genome {
            name: String::new(),
            instrument: instrument.into(),
            tags: vec!["hipotez".into(), SOURCE.into()],
            timeframe_min: (tf != 1).then_some(tf),
            entry: self.entry.clone(),
            filters: Filters {
                hours_utc: self.session_utc,
                ..Filters::default()
            },
            exit: self.exit.clone().unwrap_or(Exit {
                atr: 14,
                stop_atr: 1.5,
                target_r: Some(2.0),
                time_bars: None,
            }),
            sizing: Sizing { risk_usd },
        };
        g.validate().map_err(|e| anyhow!("genom geçersiz: {e}"))?;
        Ok(g)
    }
}

/// Temel genom + ±%25 parametre komşuları (tekrarsız, deterministik sıra), adlandırılmış.
pub fn variants(base: &Genome, id: &str) -> Vec<(String, Genome)> {
    let mut out = vec![("temel".to_string(), base.clone())];
    let mut seen = std::collections::BTreeSet::from([base.hash()]);
    for (label, g) in atlas_genome::evolve::neighbors(base, 0.25) {
        if out.len() > MAX_VARIANTS {
            break;
        }
        if g.validate().is_ok() && seen.insert(g.hash()) {
            out.push((label, g));
        }
    }
    out.into_iter()
        .enumerate()
        .map(|(i, (label, mut g))| {
            g.name = format!("{id}-v{i:02}");
            (label, g)
        })
        .collect()
}

fn note_hyp(note: &str) -> Option<(String, Value)> {
    let v: Value = serde_json::from_str(note).ok()?;
    Some((v.get("hyp")?.as_str()?.to_string(), v))
}

/// Hipotezi doğrular ve varyantlarıyla kuyruğa koyar.
pub fn submit(
    reg: &mut Registry,
    h: &Hypothesis,
    instrument: &str,
    risk_usd: u32,
    now: UnixNanos,
) -> Result<Value> {
    let base = h.genome(instrument, risk_usd)?;
    let id = format!("hip-{}", &base.hash()[..8]);
    if !reg.experiments_for_hash(&base.hash(), 1)?.is_empty() {
        bail!("bu genom arama tarafından zaten denenmiş ({id})");
    }
    let vs = variants(&base, &id);
    let mut items = Vec::with_capacity(vs.len());
    for (i, (label, g)) in vs.iter().enumerate() {
        let mut note = json!({"hyp": id, "v": i, "param": label});
        if i == 0 {
            note["hypothesis"] = serde_json::to_value(h)?;
        }
        items.push((serde_json::to_string(g)?, note.to_string()));
    }
    // Tekrar ve günlük sınır denetimi ekleme ile aynı kilitli işlemde.
    let mut used_today = 0usize;
    reg.queue_genomes_guarded(SOURCE, &items, now.0, |rows| {
        let mut today = std::collections::BTreeSet::new();
        for r in rows {
            if let Some((hid, _)) = note_hyp(&r.note) {
                if hid == id {
                    return Err(format!(
                        "{id} zaten kuyrukta/denendi (aynı hipotez ikinci kez deneme sayılmaz)"
                    ));
                }
                if r.created_ts + NANOS_PER_DAY > now.0 {
                    today.insert(hid);
                }
            }
        }
        if today.len() >= DAILY_LIMIT {
            return Err(format!(
                "günlük hipotez sınırı ({DAILY_LIMIT}) doldu; deneme sayacı şişmesin diye"
            ));
        }
        used_today = today.len();
        Ok(())
    })?
    .map_err(|e| anyhow!(e))?;
    Ok(json!({
        "id": id,
        "queued": vs.len(),
        "trials_counted": vs.len(),
        "daily_left": DAILY_LIMIT - used_today - 1,
        "genome": base,
    }))
}

/// Hipotezler ve sonuçları (en yeni önce).
pub fn list(reg: &Registry) -> Result<Value> {
    let rows = reg.queued_by_source(SOURCE)?;
    let mut out = Vec::new();
    for r in rows.iter().filter(|r| {
        note_hyp(&r.note).is_some_and(|(_, v)| v.get("v").and_then(Value::as_u64) == Some(0))
    }) {
        let Some((id, v)) = note_hyp(&r.note) else {
            continue;
        };
        let queued = rows
            .iter()
            .filter(|x| note_hyp(&x.note).is_some_and(|(h, _)| h == id))
            .count();
        let pending = rows
            .iter()
            .filter(|x| !x.taken && note_hyp(&x.note).is_some_and(|(h, _)| h == id))
            .count();
        let exps = reg.experiments_with_prefix(&format!("{id}-"), 500)?;
        let best = exps
            .iter()
            .max_by_key(|e| (e.passed, e.failed_gate.unwrap_or(11), e.trades));
        out.push(json!({
            "id": id,
            "created_ts": r.created_ts,
            "idea": v.pointer("/hypothesis/idea"),
            "timeframe": v.pointer("/hypothesis/timeframe"),
            "variants": queued,
            "pending": pending,
            "experiments": exps.len(),
            "passed": exps.iter().filter(|e| e.passed).count(),
            "best": best.map(|e| json!({"name": e.name, "passed": e.passed, "failed_gate": e.failed_gate, "trades": e.trades})),
        }));
    }
    Ok(json!({"hypotheses": out, "daily_limit": DAILY_LIMIT}))
}

pub fn run_submit(h: &Home, out: &Out, text: &str) -> Result<bool> {
    let hyp = Hypothesis::parse(text)?;
    let cfg = crate::daemon::m3_toml(h)?;
    let symbol = cfg
        .daemon
        .symbols
        .first()
        .cloned()
        .unwrap_or_else(|| "BTCUSDT".into());
    let mut reg = crate::m2::open_registry(h)?;
    let v = submit(&mut reg, &hyp, &symbol, cfg.search.risk_usd, crate::now())?;
    out.emit("hypothesis.submit", true, v.clone(), || {
        format!(
            "{} kuyrukta: {} aday (her biri deneme sayılır); bugün kalan hak {}",
            v["id"].as_str().unwrap_or_default(),
            v["queued"],
            v["daily_left"]
        )
    });
    Ok(true)
}

pub fn run_list(h: &Home, out: &Out) -> Result<bool> {
    let reg = crate::m2::open_registry(h)?;
    let v = list(&reg)?;
    out.emit("hypothesis.list", true, v.clone(), || {
        let items = v["hypotheses"].as_array().cloned().unwrap_or_default();
        if items.is_empty() {
            return "hipotez yok".into();
        }
        items
            .iter()
            .map(|x| {
                format!(
                    "{} [{}] {} — {} deney, {} bekliyor, geçen {}, en iyi {}",
                    x["id"].as_str().unwrap_or_default(),
                    x["timeframe"].as_str().unwrap_or_default(),
                    x["idea"].as_str().unwrap_or_default(),
                    x["experiments"],
                    x["pending"],
                    x["passed"],
                    x["best"]
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONDON: &str = r#"{
        "idea": "Londra açılışında Asya aralığının üstüne kapanış devam eder",
        "source": "gözlem: 2025 BTC 15m grafikleri",
        "timeframe": "15m",
        "entry": {"long": {"cross_above": ["close", "asia_high"]}},
        "session_utc": [7, 10],
        "falsification": "300+ işlemde komisyon sonrası beklenti ≤ 0"
    }"#;

    #[test]
    fn parses_builds_and_rejects_bad_input() {
        let h = Hypothesis::parse(LONDON).unwrap();
        let g = h.genome("BTCUSDT", 1000).unwrap();
        assert_eq!(g.timeframe(), 15);
        assert_eq!(g.filters.hours_utc, Some((7, 10)));
        assert_eq!(g.exit.target_r, Some(2.0));
        // Bilinmeyen alan, kötü dilim, geçersiz feature, kısa fikir reddedilir.
        assert!(Hypothesis::parse(&LONDON.replace("\"session_utc\"", "\"size\"")).is_err());
        let bad_tf = Hypothesis::parse(&LONDON.replace("\"15m\"", "\"2h\"")).unwrap();
        assert!(bad_tf.genome("BTCUSDT", 1000).is_err());
        let bad_feat = Hypothesis::parse(&LONDON.replace("asia_high", "yok_boyle")).unwrap();
        assert!(bad_feat.genome("BTCUSDT", 1000).is_err());
        assert!(
            Hypothesis::parse(&LONDON.replace(
                "Londra açılışında Asya aralığının üstüne kapanış devam eder",
                "kısa"
            ))
            .is_err()
        );
    }

    #[test]
    fn submit_queues_variants_counts_and_limits() {
        let mut reg = Registry::open_in_memory().unwrap();
        let h = Hypothesis::parse(LONDON).unwrap();
        let v = submit(&mut reg, &h, "BTCUSDT", 1000, UnixNanos(NANOS_PER_DAY)).unwrap();
        let n = v["queued"].as_u64().unwrap() as usize;
        assert!(n > 1 && n <= MAX_VARIANTS + 1, "{n}");
        // Tekrar reddedilir.
        assert!(submit(&mut reg, &h, "BTCUSDT", 1000, UnixNanos(NANOS_PER_DAY + 1)).is_err());
        // Kuyruktan çıkan ilk öğe temel genom, kaynak operatör.
        let q = reg.take_queued(0).unwrap().unwrap();
        assert_eq!(q.source, SOURCE);
        let g: Genome = serde_json::from_str(&q.genome_json).unwrap();
        assert!(g.name.ends_with("-v00"));
        // Günlük sınır: farklı hipotezlerle.
        for i in 0..DAILY_LIMIT - 1 {
            let other = h.clone();
            let mut other = other;
            other.session_utc = Some((i as u32, i as u32 + 3));
            submit(
                &mut reg,
                &other,
                "BTCUSDT",
                1000,
                UnixNanos(NANOS_PER_DAY + 10),
            )
            .unwrap();
        }
        let mut sixth = h.clone();
        sixth.session_utc = Some((20, 23));
        assert!(
            submit(
                &mut reg,
                &sixth,
                "BTCUSDT",
                1000,
                UnixNanos(NANOS_PER_DAY + 20)
            )
            .is_err()
        );
        // Ertesi gün yine açılır.
        submit(
            &mut reg,
            &sixth,
            "BTCUSDT",
            1000,
            UnixNanos(3 * NANOS_PER_DAY),
        )
        .unwrap();
        let l = list(&reg).unwrap();
        assert_eq!(l["hypotheses"].as_array().unwrap().len(), DAILY_LIMIT + 1);
    }
}
