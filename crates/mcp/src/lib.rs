//! Atlas MCP sunucusu (ADR 0009): Grok Bot'un sisteme uzaktan, araç olarak erişimi.
//!
//! - Taşıma: MCP Streamable HTTP, yalnız `POST /mcp` (JSON yanıt; SSE akışı yok, `GET` → 405).
//! - Kimlik: `Authorization: Bearer <token>` (sabit zamanlı karşılaştırma). Token yoksa sunucu açılmaz.
//! - Araçlar §6 Grok beyaz listesine birebir eşlenir ([`tools`]). `approve`, `kill-reset`, `risk set`,
//!   `deploy`, `gauntlet`, `data`, `daemon` hiç tanımlı değildir.
//! - Yürütme: [`atlas_dashboard::runner`] — kabuksuz alt süreç. Üretimde önek
//!   `sudo -n -u atlas /usr/local/lib/atlas/atlas-grok`: beyaz liste yardımcıda ve CLI'da (ATLAS_ROLE=grok)
//!   yeniden uygulanır. Sunucu süreci `grok` kullanıcısıdır; journal/sicil dosyalarına doğrudan erişemez.

pub mod tools;

use atlas_dashboard::runner::{self, CliConfig, CliOutcome};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Desteklenen MCP protokol sürümleri (en yenisi önce).
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Yanıt metninin üst sınırı (karakter); aşılırsa kısaltılır ve belirtilir.
pub const MAX_TEXT: usize = 200_000;

/// Sunucuya bağlanan modele verilen yönerge (MCP `instructions`).
pub const INSTRUCTIONS: &str = "Atlas araştırma/işlem platformunun operatör araçları. Sen operatörsün: izler, \
raporlar, sistemin stratejilerine yalnız KISITLAYICI eylem yaparsın (duraklat, düşür, kill, not). Ayrıca kendi \
ayrı sanal (paper) hesabında işlem açıp kapatabilirsin (atlas_trade_*): stop zorunlu, risk kapısı limitleri \
geçerli. Sistemin stratejilerini terfi ettiren, onay veren, limit gevşeten hiçbir araç yoktur. Her araç \
`atlas.cli/1` JSON zarfı döndürür: ok=false bir sonuçtur, isError=true ise komut çalışmamıştır.";

pub struct McpState {
    pub cli: CliConfig,
    /// Ağır araçlar (verify) için zaman aşımı.
    pub slow_timeout: Duration,
    token: Vec<u8>,
    permits: Semaphore,
    slow_permit: Semaphore,
}

impl McpState {
    /// `token` en az 32 karakter olmalı.
    pub fn new(cli: CliConfig, token: &str) -> Result<McpState, String> {
        let t = token.trim();
        if t.len() < 32 {
            return Err("MCP token en az 32 karakter olmalı (ATLAS_MCP_TOKEN)".into());
        }
        Ok(McpState {
            cli,
            slow_timeout: Duration::from_secs(900),
            token: t.as_bytes().to_vec(),
            permits: Semaphore::new(4),
            slow_permit: Semaphore::new(1),
        })
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(v) = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
        else {
            return false;
        };
        let Some(given) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        else {
            return false;
        };
        ct_eq(given.trim().as_bytes(), &self.token)
    }
}

/// Sabit zamanlı karşılaştırma (uzunluk farkı da tüm baytlar taranarak).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

pub fn router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp", post(handle).get(not_allowed).delete(not_allowed))
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .with_state(state)
}

async fn not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "yalnız POST (SSE akışı yok)",
    )
        .into_response()
}

fn rpc_error(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

async fn handle(State(st): State<Arc<McpState>>, headers: HeaderMap, body: Bytes) -> Response {
    if !st.authorized(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "yetkisiz",
        )
            .into_response();
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return axum::Json(rpc_error(Value::Null, -32700, "JSON çözümlenemedi"))
                .into_response();
        }
    };
    if msg.is_array() {
        return axum::Json(rpc_error(Value::Null, -32600, "toplu istek desteklenmiyor"))
            .into_response();
    }
    match dispatch(&st, &msg).await {
        Some(v) => axum::Json(v).into_response(),
        // Bildirim ya da yanıt: gövdesiz 202.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Tek JSON-RPC mesajını işler. Bildirim/yanıt ise `None`.
pub async fn dispatch(st: &McpState, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    let (Some(method), Some(id)) = (method, id) else {
        if method.is_none() && msg.get("id").is_none() {
            return Some(rpc_error(Value::Null, -32600, "geçersiz istek"));
        }
        return None;
    };
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(rpc_error(id, -32600, "jsonrpc 2.0 gerekli"));
    }
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let v = PROTOCOL_VERSIONS
                .iter()
                .find(|p| **p == asked)
                .copied()
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            rpc_result(
                id,
                json!({
                    "protocolVersion": v,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "atlas", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(
            id,
            json!({"tools": tools::TOOLS.iter().map(tools::Tool::listing).collect::<Vec<_>>()}),
        ),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "params.name gerekli"));
            };
            if tools::find(name).is_none() {
                return Some(rpc_error(id, -32602, &format!("bilinmeyen araç: {name}")));
            }
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            rpc_result(id, call_tool(st, name, &args).await)
        }
        _ => rpc_error(id, -32601, &format!("desteklenmeyen yöntem: {method}")),
    })
}

fn tool_text(text: String, is_error: bool, structured: Option<Value>) -> Value {
    let text = if text.chars().count() > MAX_TEXT {
        let mut t: String = text.chars().take(MAX_TEXT).collect();
        t.push_str("\n…[kısaltıldı: daha dar sorgu kullan, ör. events limit/since]");
        t
    } else {
        text
    };
    let mut v = json!({"content": [{"type": "text", "text": text}], "isError": is_error});
    if let Some(s) = structured
        && s.is_object()
    {
        v["structuredContent"] = s;
    }
    v
}

/// Aracı çalıştırır; sonuç her zaman MCP `CallToolResult`.
pub async fn call_tool(st: &McpState, name: &str, args: &Value) -> Value {
    let argv = match tools::resolve(name, args) {
        Ok(a) => a,
        Err(e) => return tool_text(format!("geçersiz argüman: {e}"), true, None),
    };
    let slow = tools::find(name).is_some_and(|t| t.slow);
    let _slow_guard = if slow {
        match st.slow_permit.try_acquire() {
            Ok(p) => Some(p),
            Err(_) => {
                return tool_text(
                    "başka bir ağır işlem (verify) sürüyor; bitince tekrar dene".into(),
                    true,
                    None,
                );
            }
        }
    } else {
        None
    };
    let Ok(_permit) = st.permits.acquire().await else {
        return tool_text("sunucu kapanıyor".into(), true, None);
    };
    let mut cfg = st.cli.clone();
    if slow {
        cfg.timeout = st.slow_timeout;
    }
    match runner::run(&cfg, &argv).await {
        CliOutcome::Envelope(v) => tool_text(v.to_string(), false, Some(v)),
        CliOutcome::CliError(v) => tool_text(v.to_string(), true, Some(v)),
        CliOutcome::Unavailable(m) | CliOutcome::Failed(m) => tool_text(m, true, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::path::PathBuf;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn state() -> Arc<McpState> {
        let cli = CliConfig::new(PathBuf::from("atlas-bu-yok-xyz"), None);
        Arc::new(McpState::new(cli, TOKEN).unwrap())
    }

    async fn post_json(st: Arc<McpState>, auth: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut b = Request::post("/mcp").header(header::CONTENT_TYPE, "application/json");
        if let Some(a) = auth {
            b = b.header(header::AUTHORIZATION, a);
        }
        let res = router(st)
            .oneshot(b.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let code = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 22)
            .await
            .unwrap();
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn bearer() -> String {
        format!("Bearer {TOKEN}")
    }

    #[test]
    fn kisa_token_reddi() {
        let cli = CliConfig::new(PathBuf::from("x"), None);
        assert!(McpState::new(cli, "kisa").is_err());
    }

    #[test]
    fn sabit_zamanli_karsilastirma() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
    }

    #[tokio::test]
    async fn yetkisiz_401() {
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        assert_eq!(
            post_json(state(), None, init.clone()).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post_json(state(), Some("Bearer yanlis"), init.clone())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post_json(state(), Some(TOKEN), init).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn initialize_ve_liste() {
        let (c, v) = post_json(
            state(),
            Some(&bearer()),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
        )
        .await;
        assert_eq!(c, StatusCode::OK);
        assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
        assert!(v["result"]["capabilities"]["tools"].is_object());

        let (_, v) = post_json(
            state(),
            Some(&bearer()),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        let names: Vec<_> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), tools::TOOLS.len());
        for forbidden in [
            "approve",
            "kill_reset",
            "gauntlet",
            "daemon",
            "deploy",
            "risk",
            "data",
        ] {
            assert!(
                !names.iter().any(|n| n.contains(forbidden)),
                "{forbidden} araç olarak açılmış"
            );
        }
    }

    #[tokio::test]
    async fn bildirim_202() {
        let (c, _) = post_json(
            state(),
            Some(&bearer()),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert_eq!(c, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn bilinmeyen_yontem_ve_arac() {
        let (_, v) = post_json(
            state(),
            Some(&bearer()),
            json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}),
        )
        .await;
        assert_eq!(v["error"]["code"], -32601);
        let (_, v) = post_json(
            state(),
            Some(&bearer()),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"atlas_approve","arguments":{}}}),
        )
        .await;
        assert_eq!(v["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn arac_hatasi_iserror() {
        // Geçersiz argüman → isError, süreç çalışmaz.
        let (_, v) = post_json(
            state(),
            Some(&bearer()),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"atlas_note","arguments":{"text":"a\u{0007}"}}}),
        )
        .await;
        assert_eq!(v["result"]["isError"], true);
        // Çalıştırılamayan ikili → isError (çökme yok).
        let (_, v) = post_json(
            state(),
            Some(&bearer()),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"atlas_status"}}),
        )
        .await;
        assert_eq!(v["result"]["isError"], true);
    }

    #[tokio::test]
    async fn get_405() {
        let res = router(state())
            .oneshot(Request::get("/mcp").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
