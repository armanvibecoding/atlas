//! İşletim katmanı (§5, §7 M3): olay hunisi (journal → uyandırma politikası → outbox), gece özeti,
//! webhook teslimi, saat kayması kontrolü.
//!
//! Tek gerçek kaynak journal'dır: her olay önce journal'a yazılır, sonra yönlendirilir. Uyandırmayan
//! olaylar (INFO, sessiz saatteki MEDIUM, dedupe) gece özetinde journal'dan okunur → hiçbiri kaybolmaz.

pub mod outbox;
pub mod policy;

use atlas_core::calendar::{format_utc, local_date};
use atlas_core::{Event, EventKind, Priority, UnixNanos};
use atlas_journal::{EventFilter, Journal};
use outbox::{Message, Outbox};
use policy::{Route, WakePolicy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error("journal: {0}")]
    Journal(#[from] atlas_journal::JournalError),
    #[error("outbox: {0}")]
    Outbox(#[from] outbox::OutboxError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SinkState {
    last_digest_seq: i64,
    last_digest_day: Option<String>,
}

/// Olay hunisi.
pub struct Sink {
    pub journal: Journal,
    pub policy: WakePolicy,
    pub outbox: Outbox,
    state_path: PathBuf,
    state: SinkState,
    batch: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct EmitReport {
    pub journaled: usize,
    pub woke: usize,
    pub batched: usize,
    pub digest: usize,
    pub deduped: usize,
}

impl Sink {
    pub fn new(
        journal: Journal,
        policy: WakePolicy,
        outbox: Outbox,
        state_path: PathBuf,
    ) -> Result<Sink, OpsError> {
        let state = match std::fs::read_to_string(&state_path) {
            Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
            Err(_) => SinkState::default(),
        };
        Ok(Sink {
            journal,
            policy,
            outbox,
            state_path,
            state,
            batch: Vec::new(),
        })
    }

    fn save_state(&self) -> Result<(), OpsError> {
        let tmp = self.state_path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.state)?)?;
        std::fs::rename(&tmp, &self.state_path)?;
        Ok(())
    }

    /// Olayları journal'a yazar ve yönlendirir. Toplu uyandırmalar tek mesajda birleşir.
    pub fn emit(&mut self, events: &[Event], now: UnixNanos) -> Result<EmitReport, OpsError> {
        let mut rep = EmitReport::default();
        if events.is_empty() {
            return Ok(rep);
        }
        let seqs = self.journal.append_all(events)?;
        rep.journaled = seqs.len();
        for (seq, e) in seqs.iter().zip(events) {
            match self.policy.route(e, now) {
                Route::Wake => {
                    self.outbox.enqueue(&Message {
                        idempotency_key: format!("ev-{seq}"),
                        created: now,
                        kind: "wake".into(),
                        body: serde_json::json!({"seq": seq, "time": format_utc(e.ts), "kind": e.kind,
                            "priority": e.priority, "key": e.key, "payload": e.payload}),
                        attempts: 0,
                        next_try: now,
                        last_error: None,
                    })?;
                    rep.woke += 1;
                }
                Route::Batched => {
                    self.batch.push(
                        serde_json::json!({"seq": seq, "kind": e.kind, "payload": e.payload}),
                    );
                    rep.batched += 1;
                }
                Route::Digest => rep.digest += 1,
                Route::Deduped => rep.deduped += 1,
            }
        }
        if !self.batch.is_empty() {
            let first = seqs.first().copied().unwrap_or(0);
            let items = std::mem::take(&mut self.batch);
            self.outbox.enqueue(&Message {
                idempotency_key: format!("batch-{first}"),
                created: now,
                kind: "wake_batch".into(),
                body: serde_json::json!({"note": "saatlik uyandırma limiti aşıldı; olaylar birleştirildi", "events": items}),
                attempts: 0,
                next_try: now,
                last_error: None,
            })?;
        }
        Ok(rep)
    }

    /// Gece özeti zamanı geldiyse son özetten bu yana tüm olayları özetleyip kuyruğa koyar.
    pub fn maybe_digest(&mut self, now: UnixNanos) -> Result<bool, OpsError> {
        if !self.policy.digest_due(now) {
            return Ok(false);
        }
        let day = local_date(now, chrono_tz::Europe::Istanbul).to_string();
        if self.state.last_digest_day.as_deref() == Some(day.as_str()) {
            return Ok(false);
        }
        let evs = self.journal.read(&EventFilter {
            after_seq: Some(self.state.last_digest_seq),
            ..Default::default()
        })?;
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        let mut notable = Vec::new();
        for s in &evs {
            *counts.entry(s.event.kind.as_str().to_string()).or_default() += 1;
            if s.event.priority != Priority::Info && notable.len() < 200 {
                notable.push(serde_json::json!({"seq": s.seq, "time": format_utc(s.event.ts), "kind": s.event.kind, "payload": s.event.payload}));
            }
        }
        let last = evs.last().map_or(self.state.last_digest_seq, |s| s.seq);
        self.outbox.enqueue(&Message {
            idempotency_key: format!("digest-{day}"),
            created: now,
            kind: "digest".into(),
            body: serde_json::json!({"day": day, "events": evs.len(), "by_kind": counts, "notable": notable,
                "from_seq": self.state.last_digest_seq, "to_seq": last}),
            attempts: 0,
            next_try: now,
            last_error: None,
        })?;
        self.state.last_digest_seq = last;
        self.state.last_digest_day = Some(day);
        self.save_state()?;
        Ok(true)
    }

    /// Webhook teslimi; ölen mesajlar `outbox_dead` olarak journal'a ve (yine) kuyruğa girer.
    pub fn deliver(
        &mut self,
        now: UnixNanos,
        sender: &dyn outbox::Sender,
    ) -> Result<outbox::DeliveryReport, OpsError> {
        let (rep, dead) = self.outbox.deliver_due(now, sender)?;
        if !dead.is_empty() {
            self.emit(&dead, now)?;
        }
        Ok(rep)
    }
}

/// Saat kayması: yerel saat ile referans sunucu saati arasındaki fark (ms). Eşik aşılırsa `clock_skew`.
pub fn clock_skew_event(local: UnixNanos, server: UnixNanos, max_ms: u64) -> Option<Event> {
    let d = local.0.abs_diff(server.0) / 1_000_000;
    (d > max_ms).then(|| {
        Event::new(
            local,
            EventKind::ClockSkew,
            serde_json::json!({"skew_ms": d, "max_ms": max_ms, "local": format_utc(local), "server": format_utc(server)}),
        )
        .with_key("clock_skew")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::calendar::parse_rfc3339;
    use std::cell::RefCell;

    struct Rec(RefCell<Vec<Message>>);
    impl outbox::Sender for Rec {
        fn send(&self, m: &Message) -> Result<(), String> {
            self.0.borrow_mut().push(m.clone());
            Ok(())
        }
    }

    fn t(s: &str) -> UnixNanos {
        parse_rfc3339(s).unwrap()
    }

    #[test]
    fn funnel_nothing_lost() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("j.sqlite")).unwrap();
        let ob = Outbox::open(dir.path().join("ob")).unwrap();
        let mut s = Sink::new(
            j,
            WakePolicy::new(Default::default()),
            ob,
            dir.path().join("st.json"),
        )
        .unwrap();
        let now = t("2026-11-01T10:00:00Z");
        let evs = vec![
            Event::new(now, EventKind::Heartbeat, serde_json::json!({})),
            Event::new(
                now,
                EventKind::KillSwitch,
                serde_json::json!({"scope": "global"}),
            )
            .with_key("kill:global"),
            Event::new(now, EventKind::DataQuality, serde_json::json!({})),
        ];
        let r = s.emit(&evs, now).unwrap();
        assert_eq!((r.journaled, r.woke, r.digest), (3, 2, 1));
        let rec = Rec(RefCell::new(vec![]));
        let d = s.deliver(now, &rec).unwrap();
        assert_eq!(d.sent, 2);
        // Özet saati (09:00 TSİ = 06:00Z) → heartbeat dahil hepsi özette.
        let dig = t("2026-11-02T06:00:30Z");
        assert!(s.maybe_digest(dig).unwrap());
        assert!(!s.maybe_digest(dig).unwrap());
        s.deliver(dig, &rec).unwrap();
        let last = rec.0.borrow().last().cloned().unwrap();
        assert_eq!(last.kind, "digest");
        assert_eq!(last.body["events"], 3);
        assert_eq!(last.body["by_kind"]["heartbeat"], 1);
    }

    #[test]
    fn skew() {
        assert!(clock_skew_event(UnixNanos(0), UnixNanos(500_000_000), 1000).is_none());
        assert!(clock_skew_event(UnixNanos(0), UnixNanos(2_000_000_000), 1000).is_some());
    }
}
