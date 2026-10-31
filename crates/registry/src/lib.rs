//! Deney sicili ve bilgi tabanı (§4.7).
//!
//! - **Her deney yazılır — başarısızlar dahil** (kural 7). Tablolar append-only (tetikleyici).
//! - Deneme sayacı global ve geri alınamaz: `experiments` satır sayısı (silme yok).
//! - DSR için tüm denemelerin Sharpe dağılımı buradan okunur.
//! - `lessons`: otopsi dersleri; yeni hipotezden önce yapısal benzerlik sorgusu.
//! - Holdout erişimleri sayılır ve kaydedilir (kural 8).

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "holdout bütçesi bitti ({used}/{budget}) — yeni holdout dönemi gerekene kadar terfi yok"
    )]
    HoldoutExhausted { used: u64, budget: u64 },
}

pub type Result<T> = std::result::Result<T, RegistryError>;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS experiments (
    id INTEGER PRIMARY KEY,
    parent_id INTEGER,
    source TEXT NOT NULL,
    name TEXT NOT NULL,
    genome_hash TEXT NOT NULL,
    genome_json TEXT NOT NULL,
    tags TEXT NOT NULL,
    data_version TEXT NOT NULL,
    fill_model TEXT NOT NULL,
    seed INTEGER NOT NULL,
    passed INTEGER NOT NULL,
    failed_gate INTEGER,
    trades INTEGER NOT NULL,
    sharpe REAL,
    report_json TEXT NOT NULL,
    report_md TEXT NOT NULL,
    cpu_ms INTEGER NOT NULL,
    llm_tokens INTEGER NOT NULL,
    created_ts INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS experiments_hash ON experiments(genome_hash);
CREATE TABLE IF NOT EXISTS lessons (
    id INTEGER PRIMARY KEY,
    experiment_id INTEGER NOT NULL,
    genome_hash TEXT NOT NULL,
    tokens TEXT NOT NULL,
    tags TEXT NOT NULL,
    what TEXT NOT NULL,
    why_died TEXT NOT NULL,
    regimes TEXT NOT NULL,
    created_ts INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS holdouts (
    id INTEGER PRIMARY KEY,
    dataset TEXT NOT NULL,
    from_ts INTEGER NOT NULL,
    to_ts INTEGER NOT NULL,
    budget INTEGER NOT NULL,
    created_ts INTEGER NOT NULL,
    UNIQUE(dataset, from_ts, to_ts)
);
CREATE TABLE IF NOT EXISTS holdout_access (
    id INTEGER PRIMARY KEY,
    holdout_id INTEGER NOT NULL,
    genome_hash TEXT NOT NULL,
    ts INTEGER NOT NULL
);
"#;

fn append_only(table: &str) -> String {
    format!(
        "CREATE TRIGGER IF NOT EXISTS {table}_no_update BEFORE UPDATE ON {table} BEGIN SELECT RAISE(ABORT, 'sicil append-only'); END;
         CREATE TRIGGER IF NOT EXISTS {table}_no_delete BEFORE DELETE ON {table} BEGIN SELECT RAISE(ABORT, 'sicil append-only'); END;"
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentRecord {
    pub parent_id: Option<i64>,
    /// insan | evrim | ajan-X/prompt-vN | referans | sahte-test
    pub source: String,
    pub name: String,
    pub genome_hash: String,
    pub genome_json: String,
    pub tags: Vec<String>,
    pub data_version: String,
    pub fill_model: String,
    pub seed: u64,
    pub passed: bool,
    pub failed_gate: Option<u32>,
    pub trades: u64,
    /// İşlem başı Sharpe (yıllıklandırılmamış).
    pub sharpe: Option<f64>,
    pub report_json: String,
    pub report_md: String,
    pub cpu_ms: u64,
    pub llm_tokens: u64,
    pub created_ts: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lesson {
    pub id: i64,
    pub experiment_id: i64,
    pub genome_hash: String,
    pub tokens: BTreeSet<String>,
    pub tags: Vec<String>,
    pub what: String,
    pub why_died: String,
    pub regimes: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub trials: u64,
    pub passed: u64,
    pub by_source: BTreeMap<String, (u64, u64)>,
    pub failed_gate_histogram: BTreeMap<String, u64>,
    pub lessons: u64,
    pub holdout_accesses: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExperimentRow {
    pub id: i64,
    pub source: String,
    pub name: String,
    pub genome_hash: String,
    pub passed: bool,
    pub failed_gate: Option<u32>,
    pub trades: u64,
    pub sharpe: Option<f64>,
    pub created_ts: u64,
}

pub struct Registry {
    conn: Connection,
}

pub fn jaccard_distance(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let inter = a.intersection(b).count() as f64;
    let uni = a.union(b).count() as f64;
    if uni == 0.0 { 0.0 } else { 1.0 - inter / uni }
}

fn to_i64(x: u64) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

impl Registry {
    pub fn open(path: &Path) -> Result<Registry> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Registry> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Registry> {
        conn.execute_batch(SCHEMA)?;
        for t in ["experiments", "lessons", "holdouts", "holdout_access"] {
            conn.execute_batch(&append_only(t))?;
        }
        Ok(Registry { conn })
    }

    /// Toplam deneme sayısı (geri alınamaz).
    pub fn trial_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM experiments", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    pub fn record(&mut self, e: &ExperimentRecord) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO experiments (parent_id, source, name, genome_hash, genome_json, tags, data_version,
               fill_model, seed, passed, failed_gate, trades, sharpe, report_json, report_md, cpu_ms, llm_tokens, created_ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                e.parent_id,
                e.source,
                e.name,
                e.genome_hash,
                e.genome_json,
                serde_json::to_string(&e.tags)?,
                e.data_version,
                e.fill_model,
                to_i64(e.seed),
                e.passed,
                e.failed_gate,
                to_i64(e.trades),
                e.sharpe,
                e.report_json,
                e.report_md,
                to_i64(e.cpu_ms),
                to_i64(e.llm_tokens),
                to_i64(e.created_ts),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Kayıtlı tüm denemelerin işlem başı Sharpe'ları (DSR'deki V[SR] için).
    pub fn sharpes(&self) -> Result<Vec<f64>> {
        let mut st = self
            .conn
            .prepare("SELECT sharpe FROM experiments WHERE sharpe IS NOT NULL ORDER BY id")?;
        let v = st
            .query_map([], |r| r.get::<_, f64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_lesson(
        &mut self,
        experiment_id: i64,
        genome_hash: &str,
        tokens: &BTreeSet<String>,
        tags: &[String],
        what: &str,
        why_died: &str,
        regimes: &str,
        ts: u64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO lessons (experiment_id, genome_hash, tokens, tags, what, why_died, regimes, created_ts)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                experiment_id,
                genome_hash,
                serde_json::to_string(tokens)?,
                serde_json::to_string(tags)?,
                what,
                why_died,
                regimes,
                to_i64(ts)
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn lessons(&self) -> Result<Vec<Lesson>> {
        let mut st = self.conn.prepare(
            "SELECT id, experiment_id, genome_hash, tokens, tags, what, why_died, regimes FROM lessons ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, eid, h, tok, tags, what, why, reg) = row?;
            out.push(Lesson {
                id,
                experiment_id: eid,
                genome_hash: h,
                tokens: serde_json::from_str(&tok)?,
                tags: serde_json::from_str(&tags)?,
                what,
                why_died: why,
                regimes: reg,
            });
        }
        Ok(out)
    }

    /// Yapısal mesafesi `max_distance` altında kalan ölü fikirler (en yakın önce).
    /// Mesafe: belirteç Jaccard'ı; etiket örtüşmesi mesafeyi %25'e kadar azaltır.
    pub fn similar_dead(
        &self,
        tokens: &BTreeSet<String>,
        tags: &[String],
        max_distance: f64,
    ) -> Result<Vec<(Lesson, f64)>> {
        let tagset: BTreeSet<&String> = tags.iter().collect();
        let mut v: Vec<(Lesson, f64)> = self
            .lessons()?
            .into_iter()
            .map(|l| {
                let d = jaccard_distance(tokens, &l.tokens);
                let lt: BTreeSet<&String> = l.tags.iter().collect();
                let tag_overlap = if tagset.is_empty() || lt.is_empty() {
                    0.0
                } else {
                    tagset.intersection(&lt).count() as f64 / tagset.union(&lt).count() as f64
                };
                let d = d * (1.0 - 0.25 * tag_overlap);
                (l, d)
            })
            .filter(|(_, d)| *d <= max_distance)
            .collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.id.cmp(&b.0.id)));
        Ok(v)
    }

    /// Holdout dönemini tanımlar (aynı aralık tekrar tanımlanırsa mevcut kimlik döner).
    pub fn ensure_holdout(
        &mut self,
        dataset: &str,
        from: u64,
        to: u64,
        budget: u64,
        ts: u64,
    ) -> Result<i64> {
        if let Some(id) = self
            .conn
            .query_row(
                // Örtüşen (ör. yeni veriyle birkaç gün kaymış) pencere aynı holdout sayılır:
                // bütçe yeni veri çekimiyle yenilenemez. Yalnız tamamen yeni (örtüşmeyen) dönem
                // yeni bütçe alır (§4.6 kapı 10: "yeni holdout dönemi").
                "SELECT id FROM holdouts WHERE dataset=?1 AND from_ts < ?3 AND to_ts > ?2 ORDER BY id DESC LIMIT 1",
                params![dataset, to_i64(from), to_i64(to)],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        self.conn.execute(
            "INSERT INTO holdouts (dataset, from_ts, to_ts, budget, created_ts) VALUES (?1,?2,?3,?4,?5)",
            params![dataset, to_i64(from), to_i64(to), to_i64(budget), to_i64(ts)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn holdout_usage(&self, holdout_id: i64) -> Result<(u64, u64)> {
        let used: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM holdout_access WHERE holdout_id=?1",
            [holdout_id],
            |r| r.get(0),
        )?;
        let budget: i64 = self.conn.query_row(
            "SELECT budget FROM holdouts WHERE id=?1",
            [holdout_id],
            |r| r.get(0),
        )?;
        Ok((
            u64::try_from(used).unwrap_or(0),
            u64::try_from(budget).unwrap_or(0),
        ))
    }

    /// Erişimi kaydeder; bütçe bittiyse kaydetmez ve hata döner.
    pub fn record_holdout_access(
        &mut self,
        holdout_id: i64,
        genome_hash: &str,
        ts: u64,
    ) -> Result<(u64, u64)> {
        let (used, budget) = self.holdout_usage(holdout_id)?;
        if used >= budget {
            return Err(RegistryError::HoldoutExhausted { used, budget });
        }
        self.conn.execute(
            "INSERT INTO holdout_access (holdout_id, genome_hash, ts) VALUES (?1,?2,?3)",
            params![holdout_id, genome_hash, to_i64(ts)],
        )?;
        Ok((used + 1, budget))
    }

    pub fn summary(&self) -> Result<Summary> {
        let trials = self.trial_count()?;
        let passed: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM experiments WHERE passed=1", [], |r| {
                    r.get(0)
                })?;
        let mut by_source = BTreeMap::new();
        {
            let mut st = self
                .conn
                .prepare("SELECT source, COUNT(*), SUM(passed) FROM experiments GROUP BY source ORDER BY source")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (s, n, p) = row?;
                by_source.insert(
                    s,
                    (u64::try_from(n).unwrap_or(0), u64::try_from(p).unwrap_or(0)),
                );
            }
        }
        let mut hist = BTreeMap::new();
        {
            let mut st = self.conn.prepare(
                "SELECT COALESCE(CAST(failed_gate AS TEXT), 'geçti'), COUNT(*) FROM experiments GROUP BY failed_gate ORDER BY failed_gate",
            )?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (g, n) = row?;
                hist.insert(g, u64::try_from(n).unwrap_or(0));
            }
        }
        let lessons: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM lessons", [], |r| r.get(0))?;
        let acc: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM holdout_access", [], |r| r.get(0))?;
        Ok(Summary {
            trials,
            passed: u64::try_from(passed).unwrap_or(0),
            by_source,
            failed_gate_histogram: hist,
            lessons: u64::try_from(lessons).unwrap_or(0),
            holdout_accesses: u64::try_from(acc).unwrap_or(0),
        })
    }

    /// Genom hash'ine göre deneyler (en yeni önce). Aşama kararları için kullanılmalı: ad değişebilir,
    /// içerik hash'i değişmez — adı aynı bırakılıp değiştirilen bir genom eski sonucu devralamaz.
    pub fn experiments_for_hash(
        &self,
        genome_hash: &str,
        limit: usize,
    ) -> Result<Vec<ExperimentRow>> {
        self.query_rows("WHERE genome_hash = ?1", Some(genome_hash), limit)
    }

    pub fn experiments(&self, limit: usize, name: Option<&str>) -> Result<Vec<ExperimentRow>> {
        self.query_rows(
            if name.is_some() {
                "WHERE name = ?1"
            } else {
                ""
            },
            name,
            limit,
        )
    }

    fn query_rows(
        &self,
        filter: &str,
        arg: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ExperimentRow>> {
        let name = arg;
        let sql = format!(
            "SELECT id, source, name, genome_hash, passed, failed_gate, trades, sharpe, created_ts FROM experiments {} ORDER BY id DESC LIMIT {limit}",
            if name.is_some() { filter } else { "" }
        );
        let mut st = self.conn.prepare(&sql)?;
        let map = |r: &rusqlite::Row<'_>| {
            Ok(ExperimentRow {
                id: r.get(0)?,
                source: r.get(1)?,
                name: r.get(2)?,
                genome_hash: r.get(3)?,
                passed: r.get(4)?,
                failed_gate: r.get(5)?,
                trades: u64::try_from(r.get::<_, i64>(6)?).unwrap_or(0),
                sharpe: r.get(7)?,
                created_ts: u64::try_from(r.get::<_, i64>(8)?).unwrap_or(0),
            })
        };
        let rows = match name {
            Some(n) => st
                .query_map([n], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => st
                .query_map([], map)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn report_md(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT report_md FROM experiments WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    #[cfg(test)]
    fn raw(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, passed: bool, gate: Option<u32>, sharpe: f64) -> ExperimentRecord {
        ExperimentRecord {
            parent_id: None,
            source: "test".into(),
            name: name.into(),
            genome_hash: format!("h-{name}"),
            genome_json: "{}".into(),
            tags: vec![],
            data_version: "v".into(),
            fill_model: "bar_fill@v1".into(),
            seed: 1,
            passed,
            failed_gate: gate,
            trades: 120,
            sharpe: Some(sharpe),
            report_json: "{}".into(),
            report_md: format!("# {name}"),
            cpu_ms: 5,
            llm_tokens: 0,
            created_ts: 1,
        }
    }

    #[test]
    fn counter_is_append_only_and_irreversible() {
        let mut r = Registry::open_in_memory().unwrap();
        r.record(&rec("a", false, Some(2), 0.01)).unwrap();
        r.record(&rec("b", true, None, 0.2)).unwrap();
        assert_eq!(r.trial_count().unwrap(), 2);
        assert!(r.raw().execute("DELETE FROM experiments", []).is_err());
        assert!(
            r.raw()
                .execute("UPDATE experiments SET passed=1", [])
                .is_err()
        );
        assert_eq!(r.trial_count().unwrap(), 2);
        assert_eq!(r.sharpes().unwrap(), vec![0.01, 0.2]);
        let s = r.summary().unwrap();
        assert_eq!(s.passed, 1);
        assert_eq!(s.failed_gate_histogram.get("2"), Some(&1));
        assert_eq!(r.experiments(10, Some("a")).unwrap().len(), 1);
        assert_eq!(r.experiments_for_hash("h-b", 10).unwrap()[0].name, "b");
        assert!(r.experiments_for_hash("h-x", 10).unwrap().is_empty());
        assert_eq!(r.report_md(1).unwrap().as_deref(), Some("# a"));
    }

    #[test]
    fn holdout_budget() {
        let mut r = Registry::open_in_memory().unwrap();
        let h = r.ensure_holdout("BTC", 10, 20, 2, 0).unwrap();
        assert_eq!(r.ensure_holdout("BTC", 10, 20, 2, 0).unwrap(), h);
        // Kaymış ama örtüşen pencere aynı bütçeyi kullanır.
        assert_eq!(r.ensure_holdout("BTC", 12, 22, 2, 0).unwrap(), h);
        assert_eq!(r.record_holdout_access(h, "x", 1).unwrap(), (1, 2));
        assert_eq!(r.record_holdout_access(h, "y", 2).unwrap(), (2, 2));
        assert!(matches!(
            r.record_holdout_access(h, "z", 3),
            Err(RegistryError::HoldoutExhausted { used: 2, budget: 2 })
        ));
        assert_eq!(r.holdout_usage(h).unwrap(), (2, 2));
        // Kaydırılmış pencereyle yeni bütçe "basılamaz".
        let h2 = r.ensure_holdout("BTC", 15, 25, 2, 0).unwrap();
        assert_eq!(h2, h);
        assert!(r.record_holdout_access(h2, "w", 4).is_err());
        // Tamamen yeni dönem yeni bütçe alır.
        let h3 = r.ensure_holdout("BTC", 30, 40, 2, 0).unwrap();
        assert_ne!(h3, h);
        assert!(r.record_holdout_access(h3, "v", 5).is_ok());
    }

    #[test]
    fn lessons_similarity() {
        let mut r = Registry::open_in_memory().unwrap();
        let t1: BTreeSet<String> = ["long", "gt(ema,ema)", "f:ema"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let t2: BTreeSet<String> = ["short", "lt(rsi,const)", "f:rsi"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        r.add_lesson(
            1,
            "h1",
            &t1,
            &["trend".into()],
            "ema kesişimi",
            "kapı 2",
            "{}",
            0,
        )
        .unwrap();
        r.add_lesson(2, "h2", &t2, &[], "rsi", "kapı 1", "{}", 0)
            .unwrap();
        let q: BTreeSet<String> = ["long", "gt(ema,ema)", "f:ema", "exit:target"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let hits = r.similar_dead(&q, &["trend".into()], 0.3).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.genome_hash, "h1");
    }
}
