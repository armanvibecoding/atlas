//! MCP araçları → `atlas` CLI argümanları (§6 Grok beyaz listesi, birebir).
//!
//! Her araç sabit bir alt komuta eşlenir; kullanıcı girdisi yalnız konumsal değer olarak ve `--`
//! ayracından SONRA geçer (bayrak olarak yorumlanamaz). Argüman listesi her zaman alt komutla başlar
//! (sudo yardımcısı `atlas-grok` ilk argümanı beyaz listeyle denetler). Kabuk kullanılmaz.

use serde_json::{Map, Value, json};

/// Bir aracın tanımı.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// Salt-okunur mu (MCP `readOnlyHint`).
    pub read_only: bool,
    /// Uzun süren ve CPU yiyen araç (zaman aşımı daha uzun).
    pub slow: bool,
    schema: fn() -> Value,
}

impl Tool {
    pub fn input_schema(&self) -> Value {
        (self.schema)()
    }

    /// `tools/list` girdisi.
    pub fn listing(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema(),
            "annotations": {
                "readOnlyHint": self.read_only,
                "destructiveHint": false,
                "openWorldHint": false,
            },
        })
    }
}

fn empty() -> Value {
    json!({"type": "object", "properties": {}, "additionalProperties": false})
}

fn id_reason() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "Strateji adı ya da genom hash öneki (≥6 karakter)", "maxLength": 128},
            "reason": {"type": "string", "description": "Gerekçe (journal'a yazılır)", "minLength": 3, "maxLength": 500},
        },
        "required": ["id", "reason"],
        "additionalProperties": false,
    })
}

/// Tüm araçlar (sıra = `tools/list` sırası).
pub const TOOLS: &[Tool] = &[
    Tool {
        name: "atlas_status",
        description: "Sistem durumu özeti: daemon kalp atışı, açık deneme, merdiven, outbox, kill switch'ler.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_health",
        description: "Sağlık kontrolü. data.healthy=false ise data.problems sorunları listeler.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_events",
        description: "Olay kuyruğu (journal). Yeni olayları kaçırmamak için son gördüğün seq ile since='seq:N' kullan.",
        read_only: true,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "since": {"type": "string", "description": "seq:N (N'den sonrakiler) ya da RFC3339 zaman", "maxLength": 40},
                    "prio": {"type": "string", "enum": ["high", "medium", "info"], "description": "Bu ve daha acil olanlar"},
                    "kind": {"type": "string", "description": "Olay türü (ör. demotion, feed_down)", "maxLength": 40},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50},
                },
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_report",
        description: "Günlük (son 24 sa) ya da haftalık (son 7 gün) rapor + metrikler.",
        read_only: true,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {"period": {"type": "string", "enum": ["daily", "weekly"]}},
                "required": ["period"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_strategies_list",
        description: "Terfi merdivenindeki stratejiler ve aşamaları.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_strategy_show",
        description: "Tek stratejinin ayrıntısı: aşama geçmişi, son gauntlet raporu.",
        read_only: true,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "Strateji adı ya da genom hash öneki", "maxLength": 128}},
                "required": ["name"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_strategy_pause",
        description: "Stratejiyi duraklat (yalnız kısıtlayıcı). Yalnız Grok'un duraklattığını Grok geri açabilir.",
        read_only: false,
        slow: false,
        schema: id_reason,
    },
    Tool {
        name: "atlas_strategy_demote",
        description: "Stratejiyi bir basamak düşür (yalnız kısıtlayıcı).",
        read_only: false,
        slow: false,
        schema: id_reason,
    },
    Tool {
        name: "atlas_strategy_resume",
        description: "Grok'un kendi duraklattığı stratejiyi geri aç. Başkasının (Arman/sistem) duraklattığı açılamaz.",
        read_only: false,
        slow: false,
        schema: id_reason,
    },
    Tool {
        name: "atlas_kill",
        description: "Kill switch tetikle. scope: global | account | strategy:<ad>. Açmak (kill-reset) yalnız Arman'dır.",
        read_only: false,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "description": "global | account | strategy:<ad>", "maxLength": 140},
                    "reason": {"type": "string", "minLength": 3, "maxLength": 500},
                },
                "required": ["scope", "reason"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_note",
        description: "Journal'a operatör notu yaz (kalıcı, silinmez).",
        read_only: false,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {"text": {"type": "string", "minLength": 1, "maxLength": 2000}},
                "required": ["text"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_experiments_summary",
        description: "Deney sicili özeti: toplam deneme sayısı, kapı dağılımı, kaynak performansı.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_budget",
        description: "LLM / veri / arama bütçesi durumu.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_approvals_list",
        description: "Bekleyen onay paketleri. Onay vermek yalnız Arman'dır; sen yalnız özetleyip Arman'a iletirsin.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_approvals_show",
        description: "Tek onay paketinin ayrıntısı.",
        read_only: true,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {"id": {"type": "integer", "minimum": 1}},
                "required": ["id"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_verify",
        description: "Kilometre taşı kabul testi (m1..m4). AĞIR: dakikalar sürebilir ve CPU yer; yalnız gerektiğinde, günde en fazla bir kez.",
        read_only: true,
        slow: true,
        schema: || {
            json!({
                "type": "object",
                "properties": {"milestone": {"type": "string", "enum": ["m1", "m2", "m3", "m4"]}},
                "required": ["milestone"],
                "additionalProperties": false,
            })
        },
    },
];

pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

fn bad(m: impl Into<String>) -> String {
    m.into()
}

/// Kontrol karakteri yok, uzunluk sınırlı, boş değil.
fn text<'a>(a: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, String> {
    let v = a
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("'{key}' (metin) gerekli")))?;
    let t = v.trim();
    if t.is_empty() {
        return Err(bad(format!("'{key}' boş")));
    }
    if t.chars().count() > max {
        return Err(bad(format!("'{key}' en fazla {max} karakter")));
    }
    if t.chars().any(char::is_control) {
        return Err(bad(format!("'{key}' kontrol karakteri içeriyor")));
    }
    Ok(t)
}

fn opt_text<'a>(
    a: &'a Map<String, Value>,
    key: &str,
    max: usize,
) -> Result<Option<&'a str>, String> {
    match a.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => text(a, key, max).map(Some),
    }
}

/// Kimlik: strateji adı / hash öneki / kapsam — dar karakter kümesi.
fn ident<'a>(a: &'a Map<String, Value>, key: &str, max: usize) -> Result<&'a str, String> {
    let t = text(a, key, max)?;
    if t.starts_with('-')
        || !t
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '#' | ':' | '.'))
    {
        return Err(bad(format!(
            "'{key}' yalnız harf, rakam ve _ - # : . içerebilir"
        )));
    }
    Ok(t)
}

fn one_of<'a>(a: &'a Map<String, Value>, key: &str, allowed: &[&str]) -> Result<&'a str, String> {
    let t = a.get(key).and_then(Value::as_str).unwrap_or_default();
    if allowed.contains(&t) {
        Ok(t)
    } else {
        Err(bad(format!(
            "'{key}' şunlardan biri olmalı: {}",
            allowed.join(", ")
        )))
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

/// Araç argümanlarını doğrular ve CLI argüman listesine çevirir.
/// Liste her zaman alt komutla başlar ve `--json` içerir.
pub fn resolve(name: &str, args: &Value) -> Result<Vec<String>, String> {
    let empty = Map::new();
    let a = match args {
        Value::Object(m) => m,
        Value::Null => &empty,
        _ => return Err(bad("arguments bir nesne olmalı")),
    };
    let tool = find(name).ok_or_else(|| bad(format!("bilinmeyen araç: {name}")))?;
    // Şemada olmayan alan reddedilir (additionalProperties: false).
    let schema = tool.input_schema();
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(k) = a.keys().find(|k| !props.contains_key(*k)) {
        return Err(bad(format!("bilinmeyen alan: {k}")));
    }
    let j = s("--json");
    let out = match name {
        "atlas_status" => vec![s("status"), j],
        "atlas_health" => vec![s("health"), j],
        "atlas_events" => {
            let mut v = vec![s("events"), j];
            if let Some(since) = opt_text(a, "since", 40)? {
                if since.starts_with('-') {
                    return Err(bad("'since' geçersiz"));
                }
                v.extend([s("--since"), s(since)]);
            }
            if a.get("prio").is_some_and(|p| !p.is_null()) {
                v.extend([
                    s("--prio"),
                    s(one_of(a, "prio", &["high", "medium", "info"])?),
                ]);
            }
            if let Some(kind) = a.get("kind").filter(|k| !k.is_null()) {
                let k = kind.as_str().unwrap_or_default();
                if k.is_empty()
                    || k.len() > 40
                    || !k.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                {
                    return Err(bad("'kind' küçük harf ve _ olmalı (ör. feed_down)"));
                }
                v.extend([s("--kind"), s(k)]);
            }
            if let Some(l) = a.get("limit").filter(|l| !l.is_null()) {
                let n = l
                    .as_u64()
                    .filter(|n| (1..=500).contains(n))
                    .ok_or_else(|| bad("'limit' 1..500 tam sayı"))?;
                v.extend([s("--limit"), n.to_string()]);
            }
            v
        }
        "atlas_report" => vec![
            s("report"),
            s(one_of(a, "period", &["daily", "weekly"])?),
            j,
        ],
        "atlas_strategies_list" => vec![s("strategies"), s("list"), j],
        "atlas_strategy_show" => vec![
            s("strategies"),
            s("show"),
            j,
            s("--"),
            s(ident(a, "name", 128)?),
        ],
        "atlas_strategy_pause" | "atlas_strategy_demote" | "atlas_strategy_resume" => {
            let action = name.trim_start_matches("atlas_strategy_");
            let id = ident(a, "id", 128)?;
            let reason = text(a, "reason", 500)?;
            vec![s("strategy"), s(action), j, s("--"), s(id), s(reason)]
        }
        "atlas_kill" => {
            let scope = ident(a, "scope", 140)?;
            let reason = text(a, "reason", 500)?;
            vec![s("kill"), j, s("--"), s(scope), s(reason)]
        }
        "atlas_note" => vec![s("note"), j, s("--"), s(text(a, "text", 2000)?)],
        "atlas_experiments_summary" => vec![s("experiments"), s("summary"), j],
        "atlas_budget" => vec![s("budget"), j],
        "atlas_approvals_list" => vec![s("approvals"), s("list"), j],
        "atlas_approvals_show" => {
            let id = a
                .get("id")
                .and_then(Value::as_i64)
                .filter(|n| *n >= 1)
                .ok_or_else(|| bad("'id' ≥1 tam sayı"))?;
            vec![s("approvals"), s("show"), j, s("--"), id.to_string()]
        }
        "atlas_verify" => vec![
            s("verify"),
            s(one_of(a, "milestone", &["m1", "m2", "m3", "m4"])?),
            j,
        ],
        _ => return Err(bad(format!("bilinmeyen araç: {name}"))),
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(name: &str, args: Value) -> Result<Vec<String>, String> {
        resolve(name, &args)
    }

    #[test]
    fn araclar_benzersiz_ve_semali() {
        let mut names: Vec<_> = TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len());
        for t in TOOLS {
            assert_eq!(t.input_schema()["type"], "object", "{}", t.name);
        }
    }

    #[test]
    fn her_arac_alt_komutla_baslar_ve_json() {
        let ok_args = json!({"period":"daily","name":"x","id":"x","reason":"neden","scope":"global","text":"t","milestone":"m1"});
        for t in TOOLS {
            let props = t.input_schema()["properties"].as_object().cloned().unwrap();
            let mut a = Map::new();
            for k in props.keys() {
                if let Some(v) = ok_args.get(k) {
                    a.insert(k.clone(), v.clone());
                }
            }
            if t.name == "atlas_approvals_show" {
                a.insert("id".into(), json!(3));
            }
            let v =
                resolve(t.name, &Value::Object(a)).unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert!(!v[0].starts_with('-'), "{}: {v:?}", t.name);
            assert!(v.contains(&"--json".to_string()), "{}", t.name);
        }
    }

    #[test]
    fn serbest_metin_ayractan_sonra() {
        let v = r("atlas_note", json!({"text": "--home=/tmp x"})).unwrap();
        assert_eq!(v, ["note", "--json", "--", "--home=/tmp x"]);
        let v = r(
            "atlas_strategy_pause",
            json!({"id": "abc123", "reason": "drift şüphesi"}),
        )
        .unwrap();
        assert_eq!(
            v,
            [
                "strategy",
                "pause",
                "--json",
                "--",
                "abc123",
                "drift şüphesi"
            ]
        );
    }

    #[test]
    fn kotu_girdiler_reddedilir() {
        assert!(r("atlas_approve", json!({})).is_err());
        assert!(r("atlas_status", json!({"x": 1})).is_err());
        assert!(
            r(
                "atlas_strategy_pause",
                json!({"id": "--home", "reason": "abc"})
            )
            .is_err()
        );
        assert!(
            r(
                "atlas_strategy_pause",
                json!({"id": "a b", "reason": "abc"})
            )
            .is_err()
        );
        assert!(r("atlas_note", json!({"text": "a\nb"})).is_err());
        assert!(r("atlas_note", json!({"text": "   "})).is_err());
        assert!(r("atlas_events", json!({"since": "--home"})).is_err());
        assert!(r("atlas_events", json!({"limit": 0})).is_err());
        assert!(r("atlas_events", json!({"limit": 501})).is_err());
        assert!(r("atlas_events", json!({"kind": "Feed-Down"})).is_err());
        assert!(r("atlas_verify", json!({"milestone": "m5; rm"})).is_err());
        assert!(r("atlas_report", json!({"period": "monthly"})).is_err());
        assert!(r("atlas_approvals_show", json!({"id": "1"})).is_err());
        assert!(r("atlas_status", json!([1])).is_err());
    }

    #[test]
    fn events_bayraklari() {
        let v = r(
            "atlas_events",
            json!({"since": "seq:42", "prio": "high", "limit": 10}),
        )
        .unwrap();
        assert_eq!(
            v,
            [
                "events", "--json", "--since", "seq:42", "--prio", "high", "--limit", "10"
            ]
        );
    }
}
