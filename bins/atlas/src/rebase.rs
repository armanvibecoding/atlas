//! `atlas journal rebase` (yalnız Arman): eski checkpoint uyuşmazlıklarını geçmişi değiştirmeden kapatır.
//!
//! Yalnız **açıklanabilen** uyuşmazlık kapatılır. Model "bayat yazıcı": yakalama düzeltmesinden önce
//! daemon açılışta journal'ı bir kez katlıyor, sonra yalnız kendi yazdıklarını görüyordu; CLI'nin
//! (Grok'un `trade open`, `note` …) ayrı bağlantıdan yazdıkları checkpoint hash'ine girmiyordu.
//! Her uyuşmazlık için o checkpoint'ten önceki son daemon açılışından sonra yazılmış yalnız-CLI
//! olayları dışlanarak hash yeniden hesaplanır; kayıtlı hash'le **birebir** (SHA-256) tutmuyorsa
//! hiçbir şey yazılmaz. Tutuyorsa doğrulanan bir `checkpoint_rebase` çapası eklenir.

use anyhow::{Result, bail};
use atlas_core::EventKind;
use atlas_journal::{EventFilter, Journal};
use serde_json::{Value, json};

/// Yalnız CLI süreçlerinin yazdığı olay türleri (daemon bunları hiç yazmaz).
pub const CLI_ONLY_KINDS: &[EventKind] = &[
    EventKind::GrokOrder,
    EventKind::OperatorNote,
    EventKind::KillReset,
    EventKind::Calibration,
    EventKind::GrokAlertSet,
    EventKind::GrokAlertCancelled,
];

#[derive(Debug, Clone, serde::Serialize)]
pub struct Explained {
    pub seq: i64,
    pub daemon_open_seq: i64,
    /// Bayat görünümün kaçırdığı olaylar.
    pub missed: Vec<i64>,
}

/// Uyuşmazlıkları modelle açıklamayı dener: (açıklananlar, açıklanamayan seq'ler).
pub fn explain(j: &Journal) -> Result<(Vec<Explained>, Vec<i64>)> {
    let r = j.replay()?;
    let opens: Vec<i64> = j
        .read(&EventFilter {
            kind: Some(EventKind::ConfigLoaded),
            ..Default::default()
        })?
        .into_iter()
        .filter(|s| s.event.payload.get("daemon").and_then(Value::as_str) == Some("başladı"))
        .map(|s| s.seq)
        .collect();
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for m in &r.mismatches {
        let Some(&open) = opens.iter().rev().find(|&&o| o < m.seq) else {
            bad.push(m.seq);
            continue;
        };
        let mut missed = Vec::new();
        let h = j.fold_hash(m.seq, |s| {
            let skip = s.seq > open && CLI_ONLY_KINDS.contains(&s.event.kind);
            if skip {
                missed.push(s.seq);
            }
            !skip
        })?;
        if h == m.recorded {
            ok.push(Explained {
                seq: m.seq,
                daemon_open_seq: open,
                missed,
            });
        } else {
            bad.push(m.seq);
        }
    }
    Ok((ok, bad))
}

/// Açıklanabilen tüm uyuşmazlıklar için çapa yazar; biri bile açıklanamazsa hiçbir şey yazmaz.
pub fn rebase(j: &mut Journal, reason: &str, ts: atlas_core::UnixNanos) -> Result<Value> {
    let (ok, bad) = explain(j)?;
    if !bad.is_empty() {
        bail!(
            "açıklanamayan checkpoint uyuşmazlığı: {bad:?} — kurcalama ya da bilinmeyen hata olabilir; çapa yazılmadı"
        );
    }
    if ok.is_empty() {
        return Ok(json!({"written": false, "reason": "uyuşmazlık yok"}));
    }
    let seqs: Vec<i64> = ok.iter().map(|e| e.seq).collect();
    let mut missed: Vec<i64> = ok.iter().flat_map(|e| e.missed.iter().copied()).collect();
    missed.sort_unstable();
    missed.dedup();
    let seq = j.rebase(
        ts,
        &seqs,
        json!({
            "model": "stale_writer",
            "cli_only_kinds": CLI_ONLY_KINDS.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            "missed_events": missed,
            "reason": reason,
        }),
        "arman",
    )?;
    Ok(json!({"written": true, "seq": seq, "supersedes": seqs, "missed_events": missed}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::{Event, UnixNanos};

    fn ev(ts: u64, kind: EventKind, p: Value) -> Event {
        Event::new(UnixNanos(ts), kind, p)
    }

    /// Eski binary'nin davranışını iki ayrı tutamakla taklit eder: daemon tutamağı yeniden
    /// açılmadan yalnız kendi projeksiyonunu bilir — burada bayat hash'i elle üretiriz.
    #[test]
    fn explains_stale_writer_and_refuses_unexplained() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("j.sqlite");
        let mut j = Journal::open(&path).unwrap();
        j.append(&ev(1, EventKind::Heartbeat, json!({}))).unwrap();
        let open = j
            .append(&ev(2, EventKind::ConfigLoaded, json!({"daemon":"başladı"})))
            .unwrap();
        j.append(&ev(3, EventKind::GrokOrder, json!({"order":{}})))
            .unwrap();
        j.append(&ev(4, EventKind::Heartbeat, json!({}))).unwrap();
        // Bayat görünüm: grok_order hariç.
        let stale_hash = j
            .fold_hash(i64::MAX, |s| s.event.kind != EventKind::GrokOrder)
            .unwrap();
        let cp = j
            .append(&ev(
                5,
                EventKind::StateCheckpoint,
                json!({"hash": stale_hash}),
            ))
            .unwrap();
        let (ok, bad) = explain(&j).unwrap();
        assert!(bad.is_empty());
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].seq, cp);
        assert_eq!(ok[0].daemon_open_seq, open);
        assert_eq!(ok[0].missed, vec![3]);

        // Açıklanamayan (rastgele) bir hash varken hiçbir şey yazılmaz.
        j.append(&ev(
            6,
            EventKind::StateCheckpoint,
            json!({"hash":"kurcalanmış"}),
        ))
        .unwrap();
        let before = j.count().unwrap();
        assert!(rebase(&mut j, "test", UnixNanos(7)).is_err());
        assert_eq!(j.count().unwrap(), before);
    }

    #[test]
    fn rebase_makes_replay_consistent() {
        let mut j = Journal::open_in_memory().unwrap();
        j.append(&ev(1, EventKind::ConfigLoaded, json!({"daemon":"başladı"})))
            .unwrap();
        j.append(&ev(2, EventKind::OperatorNote, json!({"text":"x"})))
            .unwrap();
        let stale_hash = j
            .fold_hash(i64::MAX, |s| s.event.kind != EventKind::OperatorNote)
            .unwrap();
        j.append(&ev(
            3,
            EventKind::StateCheckpoint,
            json!({"hash": stale_hash}),
        ))
        .unwrap();
        assert!(!j.replay().unwrap().consistent());
        let out = rebase(&mut j, "test", UnixNanos(4)).unwrap();
        assert_eq!(out["written"], true);
        let r = j.replay().unwrap();
        assert!(r.consistent(), "{r:?}");
        assert_eq!(r.superseded.len(), 1);
        // İkinci kez: yapacak iş yok.
        assert_eq!(
            rebase(&mut j, "test", UnixNanos(5)).unwrap()["written"],
            false
        );
    }

    /// Gerçek VPS journal'ı (2026-11-02 kopyası) varsa: 21+ uyuşmazlığın hepsi modelle birebir açıklanmalı.
    #[test]
    fn real_vps_journal_if_present() {
        let Ok(p) = std::env::var("ATLAS_TEST_VPS_JOURNAL") else {
            return;
        };
        let j = Journal::open(std::path::Path::new(&p)).unwrap();
        let r = j.replay().unwrap();
        let (ok, bad) = explain(&j).unwrap();
        eprintln!(
            "uyuşmazlık {} · açıklanan {} · açıklanamayan {:?} · ilk {:?}",
            r.mismatches.len(),
            ok.len(),
            bad,
            ok.first()
        );
        assert!(!r.mismatches.is_empty());
        assert!(bad.is_empty(), "{bad:?}");
    }
}
