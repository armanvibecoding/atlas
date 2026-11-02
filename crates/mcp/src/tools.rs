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

fn reason_only() -> Value {
    json!({
        "type": "object",
        "properties": {"reason": {"type": "string", "minLength": 3, "maxLength": 500}},
        "required": ["reason"],
        "additionalProperties": false,
    })
}

/// Ondalık fiyat/miktar metni: yalnız rakam ve tek nokta.
fn decimal<'a>(a: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    let t = text(a, key, 20)?;
    let dots = t.chars().filter(|c| *c == '.').count();
    if dots > 1 || t.starts_with('.') || !t.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(bad(format!("'{key}' ondalık sayı olmalı (ör. 67250.5)")));
    }
    Ok(t)
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
        name: "atlas_hypothesis_submit",
        description: "Strateji hipotezi gönder (yalnız fikir + kurallar; parametreleri sistem seçer). Temel genom ve             ±%25 parametre komşuları arama kuyruğuna girer, normal aday gibi tarama + gauntlet'ten geçer; her aday             deneme sayılır (DSR). Günde en fazla 5 hipotez, aynısı ikinci kez girmez. Giriş kuralları genom koşul             DSL'i: {\"long\": {\"all\": [{\"gt\": [\"ema:50\", \"ema:200\"]}, {\"cross_above\": [\"close\", \"asia_high\"]}]}}.             Sonuç: atlas_hypotheses.",
        read_only: false,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "idea": {"type": "string", "minLength": 10, "maxLength": 1000, "description": "Ne ve neden çalışmalı"},
                    "source": {"type": "string", "minLength": 3, "maxLength": 300, "description": "Fikrin kaynağı (gözlem, kitap, makale, bağlantı)"},
                    "timeframe": {"type": "string", "enum": ["1m", "5m", "15m", "30m", "1h", "4h"]},
                    "entry": {
                        "type": "object",
                        "description": "Genom koşul DSL'i: long ve/veya short. Düğümler: all/any/not, gt/lt/cross_above/cross_below [a, b]; a/b feature ('ema:20', 'close', 'asia_high', 'london_high', 'pdh', 'vwap', 'rsi:14' …; gecikme 'ema:20@1') ya da sayı.",
                        "properties": {"long": {"type": "object"}, "short": {"type": "object"}},
                        "additionalProperties": false,
                    },
                    "exit": {
                        "type": "object",
                        "description": "İsteğe bağlı başlangıç değerleri (sistem ±%25 dener). Boşsa ATR(14)×1,5 stop, 2R hedef.",
                        "properties": {
                            "atr": {"type": "integer", "minimum": 2, "maximum": 500},
                            "stop_atr": {"type": "number", "minimum": 0.05, "maximum": 50},
                            "target_r": {"type": "number", "minimum": 0.1, "maximum": 50},
                            "time_bars": {"type": "integer", "minimum": 1},
                        },
                        "required": ["atr", "stop_atr"],
                        "additionalProperties": false,
                    },
                    "session_utc": {"type": "array", "items": {"type": "integer", "minimum": 0, "maximum": 24}, "minItems": 2, "maxItems": 2, "description": "UTC saat penceresi [başlangıç, bitiş), ör. Londra açılışı [7,10], NY açılışı [13,16]"},
                    "falsification": {"type": "string", "minLength": 3, "maxLength": 500, "description": "Hipotezi ne çürütür"},
                },
                "required": ["idea", "source", "timeframe", "entry"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_hypotheses",
        description: "Gönderilen hipotezler: kaç aday, kaçı bekliyor, kaçı denendi, en iyi sonuç (düştüğü kapı).",
        read_only: true,
        slow: false,
        schema: empty,
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
        name: "atlas_market",
        description: "Canlı piyasa özeti (BTCUSDT): son fiyat, 15m/1h/4h/24h değişim, 24 sa aralık, UTC gün VWAP, 1m/15m/1h barlar ve EMA20/50, ATR14. İşlem kararından önce çağır; age_s büyükse veri bayat.",
        read_only: true,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {"bars": {"type": "integer", "minimum": 1, "maximum": 240, "default": 30, "description": "Gösterilecek son 1m bar sayısı"}},
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_trades",
        description: "Grok sanal hesabı: bakiye, equity, bugünkü PnL, açık pozisyon (stop, gerçekleşmemiş PnL), bekleyen giriş, emirlerin durumu (queued/applied/rejected/filled + red sebebi), son işlemler, limitler.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_trade_open",
        description: "Grok sanal hesabında pozisyon aç (ayrı hesap; sistemin stratejileriyle karışmaz). Stop ZORUNLU. Boyutu sen seçersin ama stopa kadar risk işlem başı tavanı ($1000) ve prop limitlerini (günlük zarar, taban) aşamaz; aşarsa reddedilir. Piyasa emri sonraki 1m barın açılışında dolar; limit verilirse o fiyattan. Hesapta aynı anda tek pozisyon. Sonucu atlas_trades ile kontrol et.",
        read_only: false,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "side": {"type": "string", "enum": ["buy", "sell"]},
                    "size": {"type": "string", "description": "BTC miktarı, 0.001 katı (ör. 0.05)", "maxLength": 20},
                    "stop": {"type": "string", "description": "Stop fiyatı (ör. 67250.5); alışta fiyatın altında, satışta üstünde", "maxLength": 20},
                    "target": {"type": "string", "description": "Hedef fiyatı (isteğe bağlı)", "maxLength": 20},
                    "limit": {"type": "string", "description": "Limit giriş fiyatı (isteğe bağlı; yoksa piyasa)", "maxLength": 20},
                    "expiry_min": {"type": "integer", "minimum": 1, "maximum": 1440, "description": "Limit emrin geçerliliği (dk, varsayılan 60)"},
                    "reason": {"type": "string", "minLength": 3, "maxLength": 500, "description": "Neden bu işlem (journal'a yazılır)"},
                },
                "required": ["side", "size", "stop", "reason"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_trade_close",
        description: "Grok hesabındaki açık pozisyonu sonraki barın açılışından piyasadan kapat.",
        read_only: false,
        slow: false,
        schema: reason_only,
    },
    Tool {
        name: "atlas_trade_cancel",
        description: "Grok hesabındaki bekleyen (dolmamış) limit girişini iptal et.",
        read_only: false,
        slow: false,
        schema: reason_only,
    },
    Tool {
        name: "atlas_alert_set",
        description: "Fiyat ya da seans alarmı kur (BTCUSDT): koşul gerçekleşince Atlas webhook'u seni uyandırır (price_alert, HIGH; sessiz saatte de). Tek seferlik. condition: price_above / price_below (price zorunlu; son fiyatın üstünde/altında olmalı; expiry_hours 1..168, varsayılan 24) ya da session_open (session: london = 07:00 UTC, ny = 09:30 US/Eastern; sonraki hafta içi açılış, açılıştan 60 dk sonra düşer). En fazla 20 etkin alarm. Emir vermez.",
        read_only: false,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "condition": {"type": "string", "enum": ["price_above", "price_below", "session_open"]},
                    "price": {"type": "string", "description": "Seviye (ör. 70250.5); price_above/price_below için", "maxLength": 20},
                    "session": {"type": "string", "enum": ["london", "ny"], "description": "session_open için"},
                    "expiry_hours": {"type": "integer", "minimum": 1, "maximum": 168, "description": "Fiyat alarmının geçerliliği (sa, varsayılan 24)"},
                    "note": {"type": "string", "maxLength": 200, "description": "Not (tetiklenince olayda geri gelir)"},
                },
                "required": ["condition"],
                "additionalProperties": false,
            })
        },
    },
    Tool {
        name: "atlas_alerts",
        description: "Alarmların: etkinler + son sonuçlananlar (triggered / cancelled / expired) ve limitler.",
        read_only: true,
        slow: false,
        schema: empty,
    },
    Tool {
        name: "atlas_alert_cancel",
        description: "Etkin alarmı iptal et (id = atlas_alert_set'in döndürdüğü alert_id).",
        read_only: false,
        slow: false,
        schema: || {
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "integer", "minimum": 1},
                    "reason": {"type": "string", "maxLength": 500},
                },
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
        "atlas_hypothesis_submit" => {
            // Yapı CLI'da şemayla doğrulanır; burada yalnız zorunlu alanlar ve boyut.
            for k in ["idea", "source", "timeframe"] {
                text(a, k, 1000)?;
            }
            one_of(a, "timeframe", &["1m", "5m", "15m", "30m", "1h", "4h"])?;
            if !a.get("entry").is_some_and(Value::is_object) {
                return Err(bad("'entry' nesne olmalı (long/short)"));
            }
            let body = serde_json::to_string(a).map_err(|e| bad(e.to_string()))?;
            if body.len() > 20_000 {
                return Err(bad("hipotez en fazla 20 KB"));
            }
            vec![s("hypothesis"), s("submit"), j, s("--"), body]
        }
        "atlas_hypotheses" => vec![s("hypothesis"), s("list"), j],
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
        "atlas_market" => {
            let mut v = vec![s("market"), j];
            if let Some(b) = a.get("bars").filter(|b| !b.is_null()) {
                let n = b
                    .as_u64()
                    .filter(|n| (1..=240).contains(n))
                    .ok_or_else(|| bad("'bars' 1..240 tam sayı"))?;
                v.extend([s("--bars"), n.to_string()]);
            }
            v
        }
        "atlas_trades" => vec![s("trade"), s("list"), j],
        "atlas_trade_open" => {
            let mut v = vec![
                s("trade"),
                s("open"),
                j,
                s("--side"),
                s(one_of(a, "side", &["buy", "sell"])?),
                s("--size"),
                s(decimal(a, "size")?),
                s("--stop"),
                s(decimal(a, "stop")?),
            ];
            if a.get("target").is_some_and(|x| !x.is_null()) {
                v.extend([s("--target"), s(decimal(a, "target")?)]);
            }
            if a.get("limit").is_some_and(|x| !x.is_null()) {
                v.extend([s("--limit"), s(decimal(a, "limit")?)]);
            }
            if let Some(e) = a.get("expiry_min").filter(|x| !x.is_null()) {
                let n = e
                    .as_u64()
                    .filter(|n| (1..=1440).contains(n))
                    .ok_or_else(|| bad("'expiry_min' 1..1440 tam sayı"))?;
                v.extend([s("--expiry-min"), n.to_string()]);
            }
            v.extend([s("--"), s(text(a, "reason", 500)?)]);
            v
        }
        "atlas_trade_close" | "atlas_trade_cancel" => {
            let op = name.trim_start_matches("atlas_trade_");
            vec![s("trade"), s(op), j, s("--"), s(text(a, "reason", 500)?)]
        }
        "atlas_alert_set" | "atlas_alerts" | "atlas_alert_cancel" => resolve_alert(name, a, j)?,
        "atlas_verify" => vec![
            s("verify"),
            s(one_of(a, "milestone", &["m1", "m2", "m3", "m4"])?),
            j,
        ],
        _ => return Err(bad(format!("bilinmeyen araç: {name}"))),
    };
    Ok(out)
}

/// Alarm araçları (ADR 0012) → `alert add|list|cancel`. Not/gerekçe yalnız `--` ayracından sonra.
fn resolve_alert(name: &str, a: &Map<String, Value>, j: String) -> Result<Vec<String>, String> {
    Ok(match name {
        "atlas_alert_set" => {
            let mut v = vec![s("alert"), s("add"), j];
            let has = |k: &str| a.get(k).is_some_and(|x| !x.is_null());
            match one_of(
                a,
                "condition",
                &["price_above", "price_below", "session_open"],
            )? {
                "session_open" => {
                    if has("price") || has("expiry_hours") {
                        return Err(bad(
                            "session_open ile 'price' / 'expiry_hours' verilmez (açılıştan 60 dk sonra düşer)",
                        ));
                    }
                    v.extend([s("--session"), s(one_of(a, "session", &["london", "ny"])?)]);
                }
                c => {
                    if has("session") {
                        return Err(bad("'session' yalnız session_open ile"));
                    }
                    let flag = if c == "price_above" {
                        "--above"
                    } else {
                        "--below"
                    };
                    v.extend([s(flag), s(decimal(a, "price")?)]);
                    if let Some(e) = a.get("expiry_hours").filter(|x| !x.is_null()) {
                        let n = e
                            .as_u64()
                            .filter(|n| (1..=168).contains(n))
                            .ok_or_else(|| bad("'expiry_hours' 1..168 tam sayı"))?;
                        v.extend([s("--expiry-hours"), n.to_string()]);
                    }
                }
            }
            if let Some(n) = opt_text(a, "note", 200)? {
                v.extend([s("--"), s(n)]);
            }
            v
        }
        "atlas_alerts" => vec![s("alert"), s("list"), j],
        "atlas_alert_cancel" => {
            let id = a
                .get("id")
                .and_then(Value::as_i64)
                .filter(|n| *n >= 1)
                .ok_or_else(|| bad("'id' ≥1 tam sayı"))?;
            let mut v = vec![s("alert"), s("cancel"), j, s("--"), id.to_string()];
            if let Some(r) = opt_text(a, "reason", 500)? {
                v.push(s(r));
            }
            v
        }
        _ => return Err(bad(format!("bilinmeyen araç: {name}"))),
    })
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
        let ok_args = json!({"period":"daily","name":"x","id":"x","reason":"neden","scope":"global","text":"t","milestone":"m1","side":"buy","size":"0.01","stop":"60000","condition":"price_above","price":"70000"});
        for t in TOOLS {
            let props = t.input_schema()["properties"].as_object().cloned().unwrap();
            let mut a = Map::new();
            for k in props.keys() {
                if let Some(v) = ok_args.get(k) {
                    a.insert(k.clone(), v.clone());
                }
            }
            if t.name == "atlas_approvals_show" || t.name == "atlas_alert_cancel" {
                a.insert("id".into(), json!(3));
            }
            if t.name == "atlas_hypothesis_submit" {
                a.insert("idea".into(), json!("Londra açılışı kırılımı devam eder"));
                a.insert("source".into(), json!("gözlem"));
                a.insert("timeframe".into(), json!("15m"));
                a.insert(
                    "entry".into(),
                    json!({"long": {"cross_above": ["close", "asia_high"]}}),
                );
            }
            let v =
                resolve(t.name, &Value::Object(a)).unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert!(!v[0].starts_with('-'), "{}: {v:?}", t.name);
            assert!(v.contains(&"--json".to_string()), "{}", t.name);
        }
    }

    #[test]
    fn hipotez_tek_json_argumani_olarak_ayractan_sonra() {
        let v = r(
            "atlas_hypothesis_submit",
            json!({"idea": "--home=/tmp kırılım", "source": "gözlem", "timeframe": "1h",
                   "entry": {"long": {"gt": ["ema:50", "ema:200"]}}}),
        )
        .unwrap();
        assert_eq!(&v[..4], ["hypothesis", "submit", "--json", "--"]);
        assert_eq!(v.len(), 5);
        assert!(v[4].starts_with('{'));
        let back: Value = serde_json::from_str(&v[4]).unwrap();
        assert_eq!(back["timeframe"], "1h");
        assert!(r("atlas_hypothesis_submit", json!({"idea": "uzun bir fikir metni", "source": "x y", "timeframe": "2h", "entry": {}})).is_err());
        assert!(r("atlas_hypothesis_submit", json!({"idea": "uzun bir fikir metni", "source": "x y", "timeframe": "1h", "entry": "ema"})).is_err());
        assert_eq!(
            r("atlas_hypotheses", json!({})).unwrap(),
            ["hypothesis", "list", "--json"]
        );
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
    fn islem_araclari() {
        let v = r(
            "atlas_trade_open",
            json!({"side":"sell","size":"0.05","stop":"70100.5","target":"68000","reason":"direnç reddi"}),
        )
        .unwrap();
        assert_eq!(
            v,
            [
                "trade",
                "open",
                "--json",
                "--side",
                "sell",
                "--size",
                "0.05",
                "--stop",
                "70100.5",
                "--target",
                "68000",
                "--",
                "direnç reddi"
            ]
        );
        assert!(
            r(
                "atlas_trade_open",
                json!({"side":"buy","size":"0.05","reason":"x y z"})
            )
            .is_err()
        );
        assert!(
            r(
                "atlas_trade_open",
                json!({"side":"buy","size":"-1","stop":"1","reason":"abc"})
            )
            .is_err()
        );
        assert!(
            r(
                "atlas_trade_open",
                json!({"side":"buy","size":"1e3","stop":"1","reason":"abc"})
            )
            .is_err()
        );
        assert!(
            r(
                "atlas_trade_open",
                json!({"side":"hold","size":"1","stop":"1","reason":"abc"})
            )
            .is_err()
        );
        assert_eq!(
            r("atlas_trade_close", json!({"reason":"hedefe yakın"})).unwrap(),
            ["trade", "close", "--json", "--", "hedefe yakın"]
        );
        assert_eq!(
            r("atlas_market", json!({"bars": 60})).unwrap(),
            ["market", "--json", "--bars", "60"]
        );
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

    #[test]
    fn alarm_araclari() {
        assert_eq!(
            r(
                "atlas_alert_set",
                json!({"condition":"price_above","price":"70250.5","expiry_hours":48,"note":"--home=/tmp kırılım"}),
            )
            .unwrap(),
            [
                "alert",
                "add",
                "--json",
                "--above",
                "70250.5",
                "--expiry-hours",
                "48",
                "--",
                "--home=/tmp kırılım"
            ]
        );
        assert_eq!(
            r(
                "atlas_alert_set",
                json!({"condition":"price_below","price":"65000"})
            )
            .unwrap(),
            ["alert", "add", "--json", "--below", "65000"]
        );
        assert_eq!(
            r(
                "atlas_alert_set",
                json!({"condition":"session_open","session":"ny"})
            )
            .unwrap(),
            ["alert", "add", "--json", "--session", "ny"]
        );
        assert_eq!(
            r("atlas_alerts", json!({})).unwrap(),
            ["alert", "list", "--json"]
        );
        assert_eq!(
            r(
                "atlas_alert_cancel",
                json!({"id": 12, "reason": "gerek kalmadı"})
            )
            .unwrap(),
            ["alert", "cancel", "--json", "--", "12", "gerek kalmadı"]
        );
        assert_eq!(
            r("atlas_alert_cancel", json!({"id": 12})).unwrap(),
            ["alert", "cancel", "--json", "--", "12"]
        );
        // Kötü girdiler: koşul/fiyat eksik ya da çelişkili, bilinmeyen seans, sınır dışı süre, kimlik metin.
        for bad in [
            json!({}),
            json!({"condition":"price_above"}),
            json!({"condition":"price_above","price":"7e4"}),
            json!({"condition":"price_above","price":"--home"}),
            json!({"condition":"price_above","price":"70000","session":"ny"}),
            json!({"condition":"session_open"}),
            json!({"condition":"session_open","session":"tokyo"}),
            json!({"condition":"session_open","session":"ny","price":"70000"}),
            json!({"condition":"session_open","session":"ny","expiry_hours":5}),
            json!({"condition":"price_below","price":"65000","expiry_hours":169}),
            json!({"condition":"price_below","price":"65000","note":"a\nb"}),
            json!({"condition":"buy","price":"65000"}),
        ] {
            assert!(r("atlas_alert_set", bad.clone()).is_err(), "{bad}");
        }
        assert!(r("atlas_alert_cancel", json!({"id": "12"})).is_err());
        assert!(r("atlas_alert_cancel", json!({"id": 0})).is_err());
        assert!(r("atlas_alerts", json!({"x": 1})).is_err());
    }
}
