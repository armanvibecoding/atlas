//! Terfi merdiveni (§4.12), onay talepleri ve arama kuyruğu. Hepsi append-only tablolar:
//! durum = en son satır. Geçmiş asla değişmez.
//!
//! `aday → gauntlet → gölge → paper → mikro eval → ölçek`
//! - Otomatik: gauntlet geçen → gölge; gölgede SPRT terfi kanıtı → paper; sapma → bir basamak düşür.
//! - İnsan onayı zorunlu: paper → mikro eval, mikro eval → ölçek (sermaye), risk limiti değişikliği.

use crate::{Registry, RegistryError, Result};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const LADDER_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS stages (
    id INTEGER PRIMARY KEY,
    genome_hash TEXT NOT NULL,
    name TEXT NOT NULL,
    stage TEXT NOT NULL,
    reason TEXT NOT NULL,
    actor TEXT NOT NULL,
    ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS stages_hash ON stages(genome_hash, id);
CREATE TABLE IF NOT EXISTS approvals (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    subject TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_ts INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS approval_decisions (
    id INTEGER PRIMARY KEY,
    approval_id INTEGER NOT NULL,
    decision TEXT NOT NULL,
    actor TEXT NOT NULL,
    ts INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS search_queue (
    id INTEGER PRIMARY KEY,
    genome_json TEXT NOT NULL,
    source TEXT NOT NULL,
    note TEXT NOT NULL,
    created_ts INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS search_queue_taken (
    id INTEGER PRIMARY KEY,
    queue_id INTEGER NOT NULL UNIQUE,
    ts INTEGER NOT NULL
);
"#;

pub const LADDER_TABLES: &[&str] = &[
    "stages",
    "approvals",
    "approval_decisions",
    "search_queue",
    "search_queue_taken",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Candidate,
    GauntletPassed,
    Shadow,
    Paper,
    MicroEval,
    Scale,
    /// Merdivenden çıkarıldı (sapma ya da insan kararı).
    Retired,
    /// Geçici duraklatma (yalnız kısıtlayıcı); devamda önceki aşamaya döner.
    Paused,
}

impl Stage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::Candidate => "candidate",
            Stage::GauntletPassed => "gauntlet_passed",
            Stage::Shadow => "shadow",
            Stage::Paper => "paper",
            Stage::MicroEval => "micro_eval",
            Stage::Scale => "scale",
            Stage::Retired => "retired",
            Stage::Paused => "paused",
        }
    }

    /// Bir basamak aşağı.
    pub fn demoted(self) -> Stage {
        match self {
            Stage::Scale => Stage::MicroEval,
            Stage::MicroEval => Stage::Paper,
            Stage::Paper => Stage::Shadow,
            _ => Stage::Retired,
        }
    }

    /// Bu basamaktan yukarı çıkış insan onayı ister mi?
    pub fn promotion_needs_approval(self) -> bool {
        matches!(self, Stage::Paper | Stage::MicroEval)
    }

    pub fn next(self) -> Option<Stage> {
        match self {
            Stage::Candidate => Some(Stage::GauntletPassed),
            Stage::GauntletPassed => Some(Stage::Shadow),
            Stage::Shadow => Some(Stage::Paper),
            Stage::Paper => Some(Stage::MicroEval),
            Stage::MicroEval => Some(Stage::Scale),
            _ => None,
        }
    }

    /// Canlı veriyle koşan aşamalar (gölge ve üstü).
    pub fn is_live(self) -> bool {
        matches!(
            self,
            Stage::Shadow | Stage::Paper | Stage::MicroEval | Stage::Scale
        )
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Stage {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        [
            Stage::Candidate,
            Stage::GauntletPassed,
            Stage::Shadow,
            Stage::Paper,
            Stage::MicroEval,
            Stage::Scale,
            Stage::Retired,
            Stage::Paused,
        ]
        .into_iter()
        .find(|x| x.as_str() == s)
        .ok_or_else(|| format!("bilinmeyen aşama: {s}"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageRow {
    pub id: i64,
    pub genome_hash: String,
    pub name: String,
    pub stage: Stage,
    pub reason: String,
    pub actor: String,
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub id: i64,
    pub kind: String,
    pub subject: String,
    pub payload: serde_json::Value,
    pub created_ts: u64,
    /// `None` = bekliyor.
    pub decision: Option<(String, String, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedGenome {
    pub id: i64,
    pub genome_json: String,
    pub source: String,
    pub note: String,
}

fn to_i64(x: u64) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

fn row_stage(r: &rusqlite::Row<'_>) -> rusqlite::Result<StageRow> {
    let st: String = r.get(3)?;
    Ok(StageRow {
        id: r.get(0)?,
        genome_hash: r.get(1)?,
        name: r.get(2)?,
        stage: st.parse().unwrap_or(Stage::Candidate),
        reason: r.get(4)?,
        actor: r.get(5)?,
        ts: u64::try_from(r.get::<_, i64>(6)?).unwrap_or(0),
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LadderError {
    #[error("izin yok: {0}")]
    Forbidden(String),
    #[error("geçersiz geçiş: {0}")]
    Invalid(String),
}

impl Registry {
    /// Bir genom hash'inin kayıtlı JSON'u (en son deneyden).
    pub fn genome_json_for_hash(&self, hash: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT genome_json FROM experiments WHERE genome_hash=?1 ORDER BY id DESC LIMIT 1",
                [hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Bir genomun son gauntlet raporu (tarama değil, kapı 1+).
    pub fn last_gauntlet_report_json(&self, hash: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT report_json FROM experiments WHERE genome_hash=?1 AND (failed_gate IS NULL OR failed_gate > 0) ORDER BY id DESC LIMIT 1",
                [hash],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_stage(
        &mut self,
        hash: &str,
        name: &str,
        stage: Stage,
        reason: &str,
        actor: &str,
        ts: u64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO stages (genome_hash, name, stage, reason, actor, ts) VALUES (?1,?2,?3,?4,?5,?6)",
            params![hash, name, stage.as_str(), reason, actor, to_i64(ts)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn current_stage(&self, hash: &str) -> Result<Option<StageRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, genome_hash, name, stage, reason, actor, ts FROM stages WHERE genome_hash=?1 ORDER BY id DESC LIMIT 1",
                [hash],
                row_stage,
            )
            .optional()?)
    }

    pub fn stage_history(&self, hash: &str) -> Result<Vec<StageRow>> {
        let mut st = self.conn.prepare(
            "SELECT id, genome_hash, name, stage, reason, actor, ts FROM stages WHERE genome_hash=?1 ORDER BY id",
        )?;
        let v = st
            .query_map([hash], row_stage)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    /// Her genomun güncel aşaması.
    pub fn all_current_stages(&self) -> Result<Vec<StageRow>> {
        let mut st = self.conn.prepare(
            "SELECT s.id, s.genome_hash, s.name, s.stage, s.reason, s.actor, s.ts FROM stages s
             JOIN (SELECT genome_hash, MAX(id) AS mid FROM stages GROUP BY genome_hash) m ON s.id = m.mid
             ORDER BY s.name",
        )?;
        let v = st
            .query_map([], row_stage)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    /// Duraklat (yalnız kısıtlayıcı; herkes yapabilir). Zaten duraklatılmışsa hata.
    pub fn pause(
        &mut self,
        hash: &str,
        reason: &str,
        actor: &str,
        ts: u64,
    ) -> std::result::Result<i64, PauseError> {
        let cur = self
            .current_stage(hash)?
            .ok_or_else(|| LadderError::Invalid("merdivende değil".into()))?;
        if cur.stage == Stage::Paused {
            return Err(LadderError::Invalid("zaten duraklatılmış".into()).into());
        }
        Ok(self.set_stage(hash, &cur.name, Stage::Paused, reason, actor, ts)?)
    }

    /// Devam: duraklatmadan önceki aşamaya döner. Grok yalnız kendi duraklattığını açabilir.
    pub fn resume(
        &mut self,
        hash: &str,
        reason: &str,
        actor: &str,
        ts: u64,
    ) -> std::result::Result<Stage, PauseError> {
        let hist = self.stage_history(hash)?;
        let last = hist
            .last()
            .ok_or_else(|| LadderError::Invalid("merdivende değil".into()))?;
        if last.stage != Stage::Paused {
            return Err(LadderError::Invalid("duraklatılmış değil".into()).into());
        }
        if actor != "arman" && last.actor != actor {
            return Err(LadderError::Forbidden(format!(
                "{actor} yalnız kendi duraklattığını açabilir (duraklatan: {})",
                last.actor
            ))
            .into());
        }
        let prev = hist
            .iter()
            .rev()
            .find(|r| r.stage != Stage::Paused)
            .map_or(Stage::Candidate, |r| r.stage);
        self.set_stage(hash, &last.name, prev, reason, actor, ts)?;
        Ok(prev)
    }

    /// Bir basamak düşür (kısıtlayıcı; Grok yapabilir).
    pub fn demote(
        &mut self,
        hash: &str,
        reason: &str,
        actor: &str,
        ts: u64,
    ) -> std::result::Result<Stage, PauseError> {
        let hist = self.stage_history(hash)?;
        let last = hist
            .last()
            .ok_or_else(|| LadderError::Invalid("merdivende değil".into()))?;
        let base = if last.stage == Stage::Paused {
            hist.iter()
                .rev()
                .find(|r| r.stage != Stage::Paused)
                .map_or(Stage::Candidate, |r| r.stage)
        } else {
            last.stage
        };
        let to = base.demoted();
        self.set_stage(hash, &last.name, to, reason, actor, ts)?;
        if last.stage == Stage::Paused && to != Stage::Retired {
            // Düşürme duraklatmayı kaldırmaz: duraklatma, ASIL duraklatanın sahipliğiyle yeniden yazılır
            // (aksi halde düşürme, başkasının duraklattığını dolaylı yoldan canlıya döndürürdü).
            self.set_stage(
                hash,
                &last.name,
                Stage::Paused,
                "düşürme sonrası duraklatma korunur",
                &last.actor,
                ts,
            )?;
            return Ok(Stage::Paused);
        }
        Ok(to)
    }

    /// Bir onay daha önce bir terfide kullanıldı mı?
    pub fn approval_consumed(&self, subject: &str, approval_id: i64) -> Result<bool> {
        let tag = format!("onay #{approval_id} ");
        Ok(self
            .stage_history(subject)?
            .iter()
            .any(|r| r.reason.starts_with(&tag)))
    }

    pub fn request_approval(
        &mut self,
        kind: &str,
        subject: &str,
        payload: &serde_json::Value,
        ts: u64,
    ) -> Result<i64> {
        // Aynı konu için bekleyen talep varsa yenisini açma.
        if let Some(a) = self
            .pending_approvals()?
            .into_iter()
            .find(|a| a.kind == kind && a.subject == subject)
        {
            return Ok(a.id);
        }
        self.conn.execute(
            "INSERT INTO approvals (kind, subject, payload, created_ts) VALUES (?1,?2,?3,?4)",
            params![kind, subject, serde_json::to_string(payload)?, to_i64(ts)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn approvals(&self) -> Result<Vec<Approval>> {
        let mut st = self.conn.prepare(
            "SELECT a.id, a.kind, a.subject, a.payload, a.created_ts, d.decision, d.actor, d.ts FROM approvals a
             LEFT JOIN approval_decisions d ON d.approval_id = a.id ORDER BY a.id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<i64>>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, subject, payload, cts, d, actor, dts) = row?;
            out.push(Approval {
                id,
                kind,
                subject,
                payload: serde_json::from_str(&payload)?,
                created_ts: u64::try_from(cts).unwrap_or(0),
                decision: match (d, actor, dts) {
                    (Some(d), Some(a), Some(t)) => Some((d, a, u64::try_from(t).unwrap_or(0))),
                    _ => None,
                },
            });
        }
        Ok(out)
    }

    pub fn pending_approvals(&self) -> Result<Vec<Approval>> {
        Ok(self
            .approvals()?
            .into_iter()
            .filter(|a| a.decision.is_none())
            .collect())
    }

    /// Karar: yalnız `arman` (çağıran anahtarı doğrulamış olmalı). Bir talep bir kez karara bağlanır.
    pub fn decide(
        &mut self,
        id: i64,
        approve: bool,
        actor: &str,
        ts: u64,
    ) -> std::result::Result<Approval, PauseError> {
        if actor != "arman" {
            return Err(LadderError::Forbidden("onay yalnız Arman".into()).into());
        }
        let a = self
            .approvals()?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| LadderError::Invalid(format!("onay #{id} yok")))?;
        if a.decision.is_some() {
            return Err(LadderError::Invalid(format!("onay #{id} zaten karara bağlanmış")).into());
        }
        self.conn.execute(
            "INSERT INTO approval_decisions (approval_id, decision, actor, ts) VALUES (?1,?2,?3,?4)",
            params![id, if approve { "approved" } else { "rejected" }, actor, to_i64(ts)],
        )?;
        Ok(self
            .approvals()?
            .into_iter()
            .find(|x| x.id == id)
            .unwrap_or(a))
    }

    pub fn queue_genome(
        &mut self,
        genome_json: &str,
        source: &str,
        note: &str,
        ts: u64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO search_queue (genome_json, source, note, created_ts) VALUES (?1,?2,?3,?4)",
            params![genome_json, source, note, to_i64(ts)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Alınmamış en eski kuyruk öğesini alır (alındı olarak işaretler).
    pub fn take_queued(&mut self, ts: u64) -> Result<Option<QueuedGenome>> {
        let q = self
            .conn
            .query_row(
                "SELECT q.id, q.genome_json, q.source, q.note FROM search_queue q
                 WHERE q.id NOT IN (SELECT queue_id FROM search_queue_taken) ORDER BY q.id LIMIT 1",
                [],
                |r| {
                    Ok(QueuedGenome {
                        id: r.get(0)?,
                        genome_json: r.get(1)?,
                        source: r.get(2)?,
                        note: r.get(3)?,
                    })
                },
            )
            .optional()?;
        if let Some(q) = &q {
            self.conn.execute(
                "INSERT INTO search_queue_taken (queue_id, ts) VALUES (?1, ?2)",
                params![q.id, to_i64(ts)],
            )?;
        }
        Ok(q)
    }

    /// Kaynak başına (deneme, taramayı geçip gauntlet'te kapı ≥ 2'ye ulaşan) — meta-öğrenme.
    pub fn source_progress(&self) -> Result<Vec<(String, u64, u64)>> {
        let mut st = self.conn.prepare(
            "SELECT source, COUNT(*), SUM(CASE WHEN failed_gate IS NULL OR failed_gate >= 2 THEN 1 ELSE 0 END)
             FROM experiments GROUP BY source ORDER BY source",
        )?;
        let v = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                    u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    /// Kaynak başına: deneme sayısı ve ulaşılan en yüksek kapı dağılımı (meta-öğrenme için).
    pub fn source_stats(&self) -> Result<Vec<(String, u64, u64, f64)>> {
        let mut st = self.conn.prepare(
            "SELECT source, COUNT(*), SUM(passed), AVG(COALESCE(failed_gate, 11)) FROM experiments GROUP BY source ORDER BY source",
        )?;
        let v = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                    u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                    r.get::<_, f64>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PauseError {
    #[error("{0}")]
    Ladder(#[from] LadderError),
    #[error("{0}")]
    Registry(#[from] RegistryError),
}

impl From<rusqlite::Error> for PauseError {
    fn from(e: rusqlite::Error) -> Self {
        PauseError::Registry(RegistryError::Sql(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_pause_resume_demote_and_permissions() {
        let mut r = Registry::open_in_memory().unwrap();
        r.set_stage("h", "s", Stage::GauntletPassed, "gauntlet", "sistem", 1)
            .unwrap();
        r.set_stage("h", "s", Stage::Shadow, "otomatik", "sistem", 2)
            .unwrap();
        r.set_stage("h", "s", Stage::Paper, "SPRT", "sistem", 3)
            .unwrap();
        r.pause("h", "test", "grok", 4).unwrap();
        assert_eq!(r.current_stage("h").unwrap().unwrap().stage, Stage::Paused);
        assert!(r.pause("h", "x", "grok", 5).is_err());
        // Başkasının duraklattığını Grok açamaz; Arman açabilir.
        r.set_stage("h2", "s2", Stage::Shadow, "x", "sistem", 1)
            .unwrap();
        r.pause("h2", "arman durdurdu", "arman", 2).unwrap();
        assert!(matches!(
            r.resume("h2", "x", "grok", 3),
            Err(PauseError::Ladder(LadderError::Forbidden(_)))
        ));
        assert_eq!(r.resume("h2", "x", "arman", 3).unwrap(), Stage::Shadow);
        // Grok kendi duraklattığını açar → önceki aşama (paper).
        assert_eq!(r.resume("h", "devam", "grok", 6).unwrap(), Stage::Paper);
        assert_eq!(r.demote("h", "sapma", "grok", 7).unwrap(), Stage::Shadow);
        assert_eq!(r.demote("h", "sapma", "sistem", 8).unwrap(), Stage::Retired);
        assert_eq!(r.all_current_stages().unwrap().len(), 2);
        // Arman'ın duraklattığı strateji Grok'un düşürmesiyle canlıya dönmez.
        r.set_stage("h3", "s3", Stage::Paper, "x", "sistem", 9)
            .unwrap();
        r.pause("h3", "arman durdurdu", "arman", 10).unwrap();
        assert_eq!(r.demote("h3", "düşür", "grok", 11).unwrap(), Stage::Paused);
        let cur = r.current_stage("h3").unwrap().unwrap();
        assert_eq!((cur.stage, cur.actor.as_str()), (Stage::Paused, "arman"));
        assert!(r.resume("h3", "x", "grok", 12).is_err());
        assert_eq!(r.resume("h3", "x", "arman", 13).unwrap(), Stage::Shadow);
        // Append-only
        assert!(r.conn.execute("DELETE FROM stages", []).is_err());
    }

    #[test]
    fn approvals_only_arman_once() {
        let mut r = Registry::open_in_memory().unwrap();
        let id = r
            .request_approval("micro_eval", "h", &serde_json::json!({"x": 1}), 1)
            .unwrap();
        assert_eq!(
            r.request_approval("micro_eval", "h", &serde_json::json!({}), 2)
                .unwrap(),
            id
        );
        assert_eq!(r.pending_approvals().unwrap().len(), 1);
        assert!(r.decide(id, true, "grok", 3).is_err());
        let a = r.decide(id, true, "arman", 3).unwrap();
        assert_eq!(a.decision.as_ref().unwrap().0, "approved");
        assert!(r.decide(id, false, "arman", 4).is_err());
        assert!(r.pending_approvals().unwrap().is_empty());
    }

    #[test]
    fn queue_fifo() {
        let mut r = Registry::open_in_memory().unwrap();
        r.queue_genome("{a}", "ajan", "n", 1).unwrap();
        r.queue_genome("{b}", "ajan", "n", 2).unwrap();
        assert_eq!(r.take_queued(3).unwrap().unwrap().genome_json, "{a}");
        assert_eq!(r.take_queued(4).unwrap().unwrap().genome_json, "{b}");
        assert!(r.take_queued(5).unwrap().is_none());
    }
}
