//! Append-only olay kaynağı (kural 6).
//!
//! - Olaylar SQLite'a eklenir; `UPDATE`/`DELETE` tetikleyicilerle **veritabanı düzeyinde** reddedilir.
//! - Durum ([`Projection`]) olaylardan katlanarak üretilir. Motor ara ara `state_checkpoint` olayı
//!   yazar (projeksiyonun hash'i); [`Journal::replay`] baştan katlayıp her checkpoint'i doğrular.
//! - Dışa aktarma SQLite backup API'siyle tutarlı anlık görüntüden yapılır (ders §9.3).

use atlas_core::{Event, EventKind, Priority, UnixNanos};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bozuk kayıt seq={seq}: {msg}")]
    Corrupt { seq: i64, msg: String },
}

pub type Result<T> = std::result::Result<T, JournalError>;

/// Ham satır: (seq, id, ts, kind, priority, key, payload).
type RawRow = (i64, String, i64, String, String, Option<String>, String);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    seq      INTEGER PRIMARY KEY,
    id       TEXT NOT NULL UNIQUE,
    ts       INTEGER NOT NULL,
    kind     TEXT NOT NULL,
    priority TEXT NOT NULL,
    key      TEXT,
    payload  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS events_kind ON events(kind);
CREATE INDEX IF NOT EXISTS events_priority ON events(priority, seq);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'journal append-only: UPDATE yasak'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'journal append-only: DELETE yasak'); END;
"#;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub seq: i64,
    pub event: Event,
}

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    pub after_seq: Option<i64>,
    pub since_ts: Option<UnixNanos>,
    /// Bu öncelik veya daha acil olanlar.
    pub min_priority: Option<Priority>,
    pub kind: Option<EventKind>,
    pub limit: Option<usize>,
    /// En yeniler önce.
    pub newest_first: bool,
}

/// Olaylardan türetilen durum. Alanlar `BTreeMap` → serileştirme deterministik.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    pub last_seq: i64,
    /// (strateji, sembol) → (yön, adet, giriş)
    pub positions: BTreeMap<String, serde_json::Value>,
    /// Gerçekleşmiş PnL − ücretler (mikro-dolar).
    pub realized_net_micros: i64,
    pub fills: u64,
    /// Aktif kill kapsamları → sebep.
    pub kills: BTreeMap<String, String>,
    pub counts: BTreeMap<String, u64>,
    pub notes: u64,
}

fn money_field(v: &serde_json::Value, k: &str) -> i64 {
    v.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0)
}

fn str_field(v: &serde_json::Value, k: &str) -> String {
    match v.get(k) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

impl Projection {
    pub fn apply(&mut self, seq: i64, e: &Event) {
        self.last_seq = seq;
        if e.kind == EventKind::StateCheckpoint {
            // Checkpoint projeksiyonu değiştirmez (kendini sayarsa hash kendine bağımlı olurdu).
            return;
        }
        *self.counts.entry(e.kind.as_str().to_string()).or_default() += 1;
        let p = &e.payload;
        match e.kind {
            EventKind::PositionOpened => {
                let key = format!(
                    "{}|{}",
                    str_field(p, "strategy"),
                    str_field(p, "instrument")
                );
                self.realized_net_micros -= money_field(p, "fee");
                self.fills += 1;
                self.positions.insert(
                    key,
                    serde_json::json!({"side": p.get("side"), "qty": p.get("qty"), "entry": p.get("price")}),
                );
            }
            EventKind::PositionClosed => {
                let key = format!(
                    "{}|{}",
                    str_field(p, "strategy"),
                    str_field(p, "instrument")
                );
                self.realized_net_micros += money_field(p, "gross") - money_field(p, "fee");
                self.fills += 1;
                self.positions.remove(&key);
            }
            EventKind::KillSwitch => {
                self.kills
                    .insert(str_field(p, "scope"), str_field(p, "reason"));
            }
            EventKind::KillReset => {
                self.kills.remove(&str_field(p, "scope"));
            }
            EventKind::OperatorNote => self.notes += 1,
            _ => {}
        }
    }

    /// Kanonik JSON'un SHA-256'sı (hex). `last_seq` hariç: checkpoint'in kendi seq'i hesaba girmez.
    pub fn hash(&self) -> String {
        let mut c = self.clone();
        c.last_seq = 0;
        let bytes = serde_json::to_vec(&c).unwrap_or_default();
        hex(&Sha256::digest(&bytes))
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub events: u64,
    pub checkpoints: u64,
    pub mismatches: Vec<CheckpointMismatch>,
    pub final_hash: String,
}

impl ReplayReport {
    pub fn consistent(&self) -> bool {
        self.mismatches.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointMismatch {
    pub seq: i64,
    pub recorded: String,
    pub recomputed: String,
}

pub struct Journal {
    conn: Connection,
    projection: Projection,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Journal> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Journal> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Journal> {
        conn.execute_batch(SCHEMA)?;
        let mut j = Journal {
            conn,
            projection: Projection::default(),
        };
        let mut p = Projection::default();
        j.for_each(|s| {
            p.apply(s.seq, &s.event);
            Ok(())
        })?;
        j.projection = p;
        Ok(j)
    }

    pub fn projection(&self) -> &Projection {
        &self.projection
    }

    /// Olayı ekler; `id` boşsa atanır. Atanan seq döner.
    pub fn append(&mut self, ev: &Event) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let seq = Self::insert(&tx, ev)?;
        tx.commit()?;
        self.projection.apply(seq, ev);
        Ok(seq)
    }

    /// Hepsi ya da hiçbiri.
    pub fn append_all(&mut self, evs: &[Event]) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        let mut seqs = Vec::with_capacity(evs.len());
        for ev in evs {
            seqs.push(Self::insert(&tx, ev)?);
        }
        tx.commit()?;
        for (s, e) in seqs.iter().zip(evs) {
            self.projection.apply(*s, e);
        }
        Ok(seqs)
    }

    fn insert(tx: &rusqlite::Transaction<'_>, ev: &Event) -> Result<i64> {
        let next: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM events", [], |r| {
            r.get(0)
        })?;
        let id = if ev.id.is_empty() {
            format!("ev-{next:012}")
        } else {
            ev.id.clone()
        };
        let ts = i64::try_from(ev.ts.0).unwrap_or(i64::MAX);
        tx.execute(
            "INSERT INTO events (seq, id, ts, kind, priority, key, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                next,
                id,
                ts,
                ev.kind.as_str(),
                ev.priority.as_str(),
                ev.key,
                serde_json::to_string(&ev.payload)?
            ],
        )?;
        Ok(next)
    }

    /// Mevcut projeksiyonun hash'iyle `state_checkpoint` yazar.
    pub fn checkpoint(&mut self, ts: UnixNanos) -> Result<i64> {
        let hash = self.projection.hash();
        let through = self.projection.last_seq;
        self.append(&Event::new(
            ts,
            EventKind::StateCheckpoint,
            serde_json::json!({"hash": hash, "through_seq": through}),
        ))
    }

    fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    }

    fn decode(row: RawRow) -> Result<StoredEvent> {
        let (seq, id, ts, kind, prio, key, payload) = row;
        let corrupt = |msg: String| JournalError::Corrupt { seq, msg };
        let kind: EventKind = kind.parse().map_err(corrupt)?;
        let priority: Priority = prio.parse().map_err(corrupt)?;
        Ok(StoredEvent {
            seq,
            event: Event {
                id,
                ts: UnixNanos(u64::try_from(ts).unwrap_or(0)),
                kind,
                priority,
                key,
                payload: serde_json::from_str(&payload)?,
            },
        })
    }

    fn for_each(&self, mut f: impl FnMut(StoredEvent) -> Result<()>) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, id, ts, kind, priority, key, payload FROM events ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([], Self::row_to_event)?;
        for r in rows {
            f(Self::decode(r?)?)?;
        }
        Ok(())
    }

    pub fn read(&self, f: &EventFilter) -> Result<Vec<StoredEvent>> {
        let mut sql =
            String::from("SELECT seq, id, ts, kind, priority, key, payload FROM events WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(s) = f.after_seq {
            sql.push_str(" AND seq > ?");
            args.push(s.into());
        }
        if let Some(t) = f.since_ts {
            sql.push_str(" AND ts >= ?");
            args.push(i64::try_from(t.0).unwrap_or(i64::MAX).into());
        }
        if let Some(p) = f.min_priority {
            let allowed: Vec<&str> = [Priority::High, Priority::Medium, Priority::Info]
                .into_iter()
                .filter(|x| x.at_least(p))
                .map(Priority::as_str)
                .collect();
            sql.push_str(&format!(
                " AND priority IN ({})",
                allowed.iter().map(|_| "?").collect::<Vec<_>>().join(",")
            ));
            for a in allowed {
                args.push(a.to_string().into());
            }
        }
        if let Some(k) = f.kind {
            sql.push_str(" AND kind = ?");
            args.push(k.as_str().to_string().into());
        }
        sql.push_str(if f.newest_first {
            " ORDER BY seq DESC"
        } else {
            " ORDER BY seq ASC"
        });
        if let Some(l) = f.limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), Self::row_to_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(Self::decode(r?)?);
        }
        Ok(out)
    }

    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    pub fn last(&self) -> Result<Option<StoredEvent>> {
        let row = self
            .conn
            .query_row(
                "SELECT seq, id, ts, kind, priority, key, payload FROM events ORDER BY seq DESC LIMIT 1",
                [],
                Self::row_to_event,
            )
            .optional()?;
        row.map(Self::decode).transpose()
    }

    /// Baştan katlar; her `state_checkpoint`'in kaydettiği hash'i yeniden hesaplananla karşılaştırır.
    pub fn replay(&self) -> Result<ReplayReport> {
        let mut p = Projection::default();
        let mut events = 0u64;
        let mut checkpoints = 0u64;
        let mut mismatches = Vec::new();
        self.for_each(|s| {
            events += 1;
            if s.event.kind == EventKind::StateCheckpoint {
                checkpoints += 1;
                let recorded = str_field(&s.event.payload, "hash");
                let recomputed = p.hash();
                if recorded != recomputed {
                    mismatches.push(CheckpointMismatch {
                        seq: s.seq,
                        recorded,
                        recomputed,
                    });
                }
            }
            p.apply(s.seq, &s.event);
            Ok(())
        })?;
        Ok(ReplayReport {
            events,
            checkpoints,
            mismatches,
            final_hash: p.hash(),
        })
    }

    /// Tutarlı anlık görüntüyü `dest`'e yazar (SQLite online backup API).
    pub fn export_snapshot(&self, dest: &Path) -> Result<()> {
        let mut out = Connection::open(dest)?;
        let backup = rusqlite::backup::Backup::new(&self.conn, &mut out)?;
        backup.run_to_completion(256, std::time::Duration::from_millis(0), None)?;
        Ok(())
    }

    /// Ham bağlantı yalnız testlerde (append-only tetikleyicilerini denemek için).
    #[cfg(test)]
    fn raw(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: u64, kind: EventKind, payload: serde_json::Value) -> Event {
        Event::new(UnixNanos(ts), kind, payload)
    }

    fn sample(j: &mut Journal) {
        j.append(&ev(
            1,
            EventKind::PositionOpened,
            serde_json::json!({"strategy":"a","instrument":"NQ","side":"buy","qty":1,"price":100,"fee":2_000_000}),
        ))
        .unwrap();
        j.append(&ev(2, EventKind::Heartbeat, serde_json::json!({})))
            .unwrap();
        j.append(&ev(
            3,
            EventKind::PositionClosed,
            serde_json::json!({"strategy":"a","instrument":"NQ","gross":50_000_000,"fee":2_000_000}),
        ))
        .unwrap();
        j.append(&ev(
            4,
            EventKind::KillSwitch,
            serde_json::json!({"scope":"global","reason":"test"}),
        ))
        .unwrap();
    }

    #[test]
    fn update_and_delete_are_rejected_by_db() {
        let mut j = Journal::open_in_memory().unwrap();
        sample(&mut j);
        assert!(j.raw().execute("UPDATE events SET kind='x'", []).is_err());
        assert!(j.raw().execute("DELETE FROM events", []).is_err());
        assert_eq!(j.count().unwrap(), 4);
    }

    #[test]
    fn projection_folds_events() {
        let mut j = Journal::open_in_memory().unwrap();
        sample(&mut j);
        let p = j.projection();
        assert!(p.positions.is_empty());
        assert_eq!(p.realized_net_micros, 50_000_000 - 4_000_000);
        assert_eq!(p.kills.get("global").map(String::as_str), Some("test"));
        assert_eq!(p.counts.get("heartbeat"), Some(&1));
    }

    #[test]
    fn replay_consistent_and_detects_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("j.sqlite");
        {
            let mut j = Journal::open(&path).unwrap();
            sample(&mut j);
            j.checkpoint(UnixNanos(5)).unwrap();
            j.append(&ev(
                6,
                EventKind::OperatorNote,
                serde_json::json!({"text":"x"}),
            ))
            .unwrap();
            j.checkpoint(UnixNanos(7)).unwrap();
            let r = j.replay().unwrap();
            assert!(r.consistent(), "{r:?}");
            assert_eq!(r.checkpoints, 2);
            assert_eq!(r.final_hash, j.projection().hash());
        }
        // Yeniden açınca projeksiyon aynen kurulur.
        let j = Journal::open(&path).unwrap();
        assert_eq!(j.projection().notes, 1);
        assert!(j.replay().unwrap().consistent());
        // Tetikleyiciler kaldırılıp kayıt değiştirilirse replay yakalar.
        j.raw()
            .execute_batch("DROP TRIGGER events_no_update; UPDATE events SET payload='{\"strategy\":\"a\",\"instrument\":\"NQ\",\"gross\":1,\"fee\":0}' WHERE seq=3;")
            .unwrap();
        let r = j.replay().unwrap();
        assert!(!r.consistent());
    }

    #[test]
    fn filters_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = Journal::open(&dir.path().join("a.sqlite")).unwrap();
        sample(&mut j);
        let high = j
            .read(&EventFilter {
                min_priority: Some(Priority::High),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].event.kind, EventKind::KillSwitch);
        let med = j
            .read(&EventFilter {
                min_priority: Some(Priority::Medium),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(med.len(), 3);
        let after = j
            .read(&EventFilter {
                after_seq: Some(2),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(after.len(), 2);
        let newest = j
            .read(&EventFilter {
                newest_first: true,
                limit: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(newest[0].seq, 4);
        assert_eq!(j.last().unwrap().unwrap().seq, 4);
        let snap = dir.path().join("snap.sqlite");
        j.export_snapshot(&snap).unwrap();
        let j2 = Journal::open(&snap).unwrap();
        assert_eq!(j2.count().unwrap(), 4);
        assert_eq!(j2.projection(), j.projection());
    }
}
