//! Disk tabanlı outbox (§5): webhook teslimi `Idempotency-Key` ile, üstel geri çekilme, ölü kutusu.
//!
//! Dizinler: `pending/`, `sent/`, `dead/`. Her mesaj bir JSON dosyası; yazım atomik (geçici + rename).
//! Süreç çökse bile bekleyen mesajlar diskte kalır (ders §9.1). Hiçbir mesaj sessizce silinmez:
//! teslim edilemeyen `dead/`'e taşınır ve `outbox_dead` (HIGH) olayı üretilir.

use atlas_core::units::NANOS_PER_SEC;
use atlas_core::{Event, EventKind, UnixNanos};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error("io {0}: {1}")]
    Io(String, std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

fn io(p: &Path) -> impl FnOnce(std::io::Error) -> OutboxError + '_ {
    move |e| OutboxError::Io(p.display().to_string(), e)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Teslim tekilliği: alıcı aynı anahtarı ikinci kez işlememeli.
    pub idempotency_key: String,
    pub created: UnixNanos,
    /// `wake` = hemen uyandır; `digest` = gece özeti.
    pub kind: String,
    pub body: serde_json::Value,
    pub attempts: u32,
    pub next_try: UnixNanos,
    pub last_error: Option<String>,
}

/// Teslim aracı (HTTP ya da test sahtesi).
pub trait Sender {
    /// `Ok(())` = alıcı kabul etti (2xx). `Err` = yeniden denenecek.
    fn send(&self, msg: &Message) -> Result<(), String>;
    fn configured(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub base_secs: u64,
    pub max_backoff_secs: u64,
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // 30 sn, 60, 120 … en fazla 1 sa aralık; 24 deneme ≈ 1 gün sonra ölü kutusu.
        RetryPolicy {
            base_secs: 30,
            max_backoff_secs: 3600,
            max_attempts: 24,
        }
    }
}

impl RetryPolicy {
    pub fn backoff(&self, attempts: u32) -> u64 {
        let exp = self
            .base_secs
            .saturating_mul(1u64 << attempts.saturating_sub(1).min(20));
        exp.min(self.max_backoff_secs)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DeliveryReport {
    pub sent: usize,
    pub retried: usize,
    pub dead: usize,
    pub not_due: usize,
    pub paused_unconfigured: bool,
}

pub struct Outbox {
    root: PathBuf,
    pub retry: RetryPolicy,
}

impl Outbox {
    pub fn open(root: impl Into<PathBuf>) -> Result<Outbox, OutboxError> {
        let root = root.into();
        for d in ["pending", "sent", "dead"] {
            let p = root.join(d);
            fs::create_dir_all(&p).map_err(io(&p))?;
        }
        Ok(Outbox {
            root,
            retry: RetryPolicy::default(),
        })
    }

    fn file_name(m: &Message) -> String {
        let safe: String = m
            .idempotency_key
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(80)
            .collect();
        format!("{:020}-{safe}.json", m.created.0)
    }

    fn write_atomic(path: &Path, m: &Message) -> Result<(), OutboxError> {
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(m)?).map_err(io(&tmp))?;
        fs::rename(&tmp, path).map_err(io(path))
    }

    /// Kuyruğa ekler. Aynı anahtar bekliyorsa ikinci kez eklenmez (dedupe), `false` döner.
    pub fn enqueue(&self, m: &Message) -> Result<bool, OutboxError> {
        let name = Self::file_name(m);
        for d in ["pending", "sent", "dead"] {
            if self.root.join(d).join(&name).exists() {
                return Ok(false);
            }
        }
        Self::write_atomic(&self.root.join("pending").join(name), m)?;
        Ok(true)
    }

    fn list(&self, dir: &str) -> Result<Vec<(PathBuf, Message)>, OutboxError> {
        let p = self.root.join(dir);
        let mut v = Vec::new();
        for e in fs::read_dir(&p).map_err(io(&p))? {
            let e = e.map_err(io(&p))?;
            let path = e.path();
            if path.extension().is_some_and(|x| x == "json") {
                let text = fs::read_to_string(&path).map_err(io(&path))?;
                v.push((path, serde_json::from_str(&text)?));
            }
        }
        v.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(v)
    }

    pub fn pending(&self) -> Result<Vec<Message>, OutboxError> {
        Ok(self.list("pending")?.into_iter().map(|x| x.1).collect())
    }

    pub fn counts(&self) -> Result<(usize, usize, usize), OutboxError> {
        Ok((
            self.list("pending")?.len(),
            self.list("sent")?.len(),
            self.list("dead")?.len(),
        ))
    }

    /// Vadesi gelen mesajları teslim etmeye çalışır. Ölen her mesaj için `outbox_dead` olayı döner
    /// (çağıran journal'a yazar ve — mümkünse — yine kuyruğa koyar).
    pub fn deliver_due(
        &self,
        now: UnixNanos,
        sender: &dyn Sender,
    ) -> Result<(DeliveryReport, Vec<Event>), OutboxError> {
        let mut rep = DeliveryReport::default();
        let mut events = Vec::new();
        if !sender.configured() {
            rep.paused_unconfigured = true;
            rep.not_due = self.list("pending")?.len();
            return Ok((rep, events));
        }
        for (path, mut m) in self.list("pending")? {
            if m.next_try > now {
                rep.not_due += 1;
                continue;
            }
            m.attempts += 1;
            match sender.send(&m) {
                Ok(()) => {
                    let dest = self
                        .root
                        .join("sent")
                        .join(path.file_name().unwrap_or_default());
                    Self::write_atomic(&dest, &m)?;
                    fs::remove_file(&path).map_err(io(&path))?;
                    rep.sent += 1;
                }
                Err(e) => {
                    m.last_error = Some(e.chars().take(500).collect());
                    if m.attempts >= self.retry.max_attempts {
                        let dest = self
                            .root
                            .join("dead")
                            .join(path.file_name().unwrap_or_default());
                        Self::write_atomic(&dest, &m)?;
                        fs::remove_file(&path).map_err(io(&path))?;
                        rep.dead += 1;
                        events.push(
                            Event::new(
                                now,
                                EventKind::OutboxDead,
                                serde_json::json!({"idempotency_key": m.idempotency_key, "attempts": m.attempts, "error": m.last_error}),
                            )
                            .with_key(format!("outbox_dead:{}", m.idempotency_key)),
                        );
                    } else {
                        m.next_try = now.plus(self.retry.backoff(m.attempts) * NANOS_PER_SEC);
                        Self::write_atomic(&path, &m)?;
                        rep.retried += 1;
                    }
                }
            }
        }
        Ok((rep, events))
    }
}

/// Hata metninden yapılandırılmış URL'yi (ve sorgu dizesini) temizler — ikinci savunma katmanı.
fn redact_url(msg: &str, url: &str) -> String {
    let mut out = msg.to_string();
    let base = url.split('?').next().unwrap_or(url);
    for s in [url, base] {
        if !s.is_empty() {
            out = out.replace(s, "<webhook>");
        }
    }
    out
}

/// HTTP webhook teslimi (POST JSON, `Idempotency-Key` başlığı). URL boşsa "yapılandırılmamış".
pub struct HttpSender {
    url: String,
    client: Option<reqwest::blocking::Client>,
    /// İsteğe bağlı kimlik doğrulama başlığı (ad, değer) — ör. `Authorization: Bearer …`.
    auth: Option<(String, String)>,
}

impl HttpSender {
    pub fn new(url: &str) -> HttpSender {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .ok();
        HttpSender {
            url: url.trim().to_string(),
            client,
            auth: None,
        }
    }

    /// Kimlik doğrulama başlığı ekler. `scheme` boşsa değer yalın anahtardır, değilse `"<scheme> <key>"`.
    /// Anahtar boşsa başlık eklenmez.
    pub fn with_auth(mut self, header: &str, scheme: &str, key: &str) -> HttpSender {
        let (header, scheme, key) = (header.trim(), scheme.trim(), key.trim());
        if !key.is_empty() && !header.is_empty() {
            let value = if scheme.is_empty() {
                key.to_string()
            } else {
                format!("{scheme} {key}")
            };
            self.auth = Some((header.to_string(), value));
        }
        self
    }

    pub fn has_auth(&self) -> bool {
        self.auth.is_some()
    }
}

impl Sender for HttpSender {
    fn configured(&self) -> bool {
        !self.url.is_empty() && self.client.is_some()
    }

    fn send(&self, m: &Message) -> Result<(), String> {
        let c = self.client.as_ref().ok_or("http istemcisi yok")?;
        if !self.url.starts_with("https://") && !self.url.starts_with("http://") {
            return Err("webhook URL http(s) değil".into());
        }
        let mut req = c
            .post(&self.url)
            .header("Idempotency-Key", &m.idempotency_key);
        if let Some((h, v)) = &self.auth {
            req = req.header(h.as_str(), v.as_str());
        }
        let r = req
            .json(&serde_json::json!({"schema": "atlas.webhook/1", "kind": m.kind, "created": m.created.0, "body": m.body}))
            .send()
            // URL gizli olabilir (anahtar/yol içerir): hata metnine girmez, çünkü last_error ve
            // outbox_dead olayları journal'a yazılır ve Grok'un okuma araçlarıyla görünür.
            .map_err(|e| redact_url(&e.without_url().to_string(), &self.url))?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {}", r.status()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        fail: RefCell<u32>,
        got: RefCell<Vec<String>>,
    }
    impl Sender for Fake {
        fn send(&self, m: &Message) -> Result<(), String> {
            let mut f = self.fail.borrow_mut();
            if *f > 0 {
                *f -= 1;
                return Err("503".into());
            }
            self.got.borrow_mut().push(m.idempotency_key.clone());
            Ok(())
        }
    }

    fn msg(k: &str, t: u64) -> Message {
        Message {
            idempotency_key: k.into(),
            created: UnixNanos(t),
            kind: "wake".into(),
            body: serde_json::json!({}),
            attempts: 0,
            next_try: UnixNanos(t),
            last_error: None,
        }
    }

    #[test]
    fn delivers_with_backoff_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let ob = Outbox::open(dir.path()).unwrap();
        assert!(ob.enqueue(&msg("a", 1)).unwrap());
        assert!(!ob.enqueue(&msg("a", 1)).unwrap());
        let s = Fake {
            fail: RefCell::new(2),
            got: RefCell::new(vec![]),
        };
        let (r, _) = ob.deliver_due(UnixNanos(10), &s).unwrap();
        assert_eq!(r.retried, 1);
        // Geri çekilme süresi dolmadan tekrar denenmez.
        let (r, _) = ob.deliver_due(UnixNanos(11), &s).unwrap();
        assert_eq!(r.not_due, 1);
        let later = UnixNanos(10 + 31 * NANOS_PER_SEC);
        let (r, _) = ob.deliver_due(later, &s).unwrap();
        assert_eq!(r.retried, 1);
        let (r, _) = ob
            .deliver_due(later.plus(3600 * NANOS_PER_SEC), &s)
            .unwrap();
        assert_eq!(r.sent, 1);
        assert_eq!(ob.counts().unwrap(), (0, 1, 0));
        assert_eq!(s.got.borrow().as_slice(), ["a"]);
        // Gönderilmiş anahtar yeniden kuyruğa giremez.
        assert!(!ob.enqueue(&msg("a", 1)).unwrap());
    }

    #[test]
    fn dead_letter_after_max_attempts_emits_event() {
        let dir = tempfile::tempdir().unwrap();
        let mut ob = Outbox::open(dir.path()).unwrap();
        ob.retry.max_attempts = 2;
        ob.enqueue(&msg("x", 1)).unwrap();
        let s = Fake {
            fail: RefCell::new(100),
            got: RefCell::new(vec![]),
        };
        let (_, e1) = ob.deliver_due(UnixNanos(5), &s).unwrap();
        assert!(e1.is_empty());
        let (r, e2) = ob.deliver_due(UnixNanos(u64::MAX / 2), &s).unwrap();
        assert_eq!(r.dead, 1);
        assert_eq!(e2[0].kind, EventKind::OutboxDead);
        assert_eq!(ob.counts().unwrap(), (0, 0, 1));
    }

    #[test]
    fn unconfigured_pauses_without_losing() {
        let dir = tempfile::tempdir().unwrap();
        let ob = Outbox::open(dir.path()).unwrap();
        ob.enqueue(&msg("y", 1)).unwrap();
        let h = HttpSender::new("");
        let (r, e) = ob.deliver_due(UnixNanos(5), &h).unwrap();
        assert!(r.paused_unconfigured);
        assert!(e.is_empty());
        assert_eq!(ob.counts().unwrap(), (1, 0, 0));
    }

    #[test]
    fn transport_error_never_contains_url() {
        // Bağlanılamayan yerel port: hata metni URL'yi (gizli yol/sorgu) içermemeli.
        let url = "http://127.0.0.1:9/gizli-yol-abc123?token=sir";
        let h = HttpSender::new(url).with_auth("Authorization", "Bearer", "k");
        let e = h.send(&msg("z", 1)).unwrap_err();
        assert!(!e.contains("gizli-yol-abc123"), "{e}");
        assert!(!e.contains("sir"), "{e}");
        assert_eq!(
            redact_url("x http://h/p?q=1 y http://h/p z", "http://h/p?q=1"),
            "x <webhook> y <webhook> z"
        );
    }

    #[test]
    fn auth_header_only_with_key() {
        let h = HttpSender::new("http://x").with_auth("Authorization", "Bearer", "");
        assert!(!h.has_auth());
        let h = HttpSender::new("http://x").with_auth("Authorization", "Bearer", " k1 ");
        assert_eq!(h.auth, Some(("Authorization".into(), "Bearer k1".into())));
        let h = HttpSender::new("http://x").with_auth("X-Api-Key", "", "k2");
        assert_eq!(h.auth, Some(("X-Api-Key".into(), "k2".into())));
    }

    #[test]
    fn backoff_grows_and_caps() {
        let p = RetryPolicy::default();
        assert_eq!(p.backoff(1), 30);
        assert_eq!(p.backoff(2), 60);
        assert_eq!(p.backoff(30), 3600);
    }
}
