//! Axum yönlendiricisi: gömülü panel + `GET /api/<komut>` JSON API.

use crate::commands::{self, ApiError, Param, QuerySpec, READ_COMMANDS};
use crate::runner::{self, CliConfig, CliOutcome};
use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Dashboard'un kendi hata/dizin yanıtlarının şeması (CLI zarfından ayırt etmek için).
pub const DASHBOARD_SCHEMA: &str = "atlas.dashboard/1";

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const APP_CSS: &str = include_str!("../assets/app.css");

/// Aynı anda en fazla bu kadar CLI alt süreci (otomatik yenileme süreç yığmasın).
pub const MAX_CONCURRENT_CLI: usize = 4;

/// Paylaşılan durum.
pub struct AppState {
    pub cli: CliConfig,
    /// Kabul edilen `Host` başlıkları (küçük harf). DNS rebinding'e karşı: tarayıcı başka bir
    /// alan adı üzerinden 127.0.0.1'e yönlendirilse bile Host başlığı tutmaz → 421.
    pub allowed_hosts: Vec<String>,
    permits: Semaphore,
}

impl AppState {
    pub fn new(cli: CliConfig, allowed_hosts: Vec<String>) -> AppState {
        AppState {
            cli,
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(|h| h.to_ascii_lowercase())
                .collect(),
            permits: Semaphore::new(MAX_CONCURRENT_CLI),
        }
    }
}

/// Bir port için localhost Host başlığı varyantları.
pub fn loopback_hosts(port: u16) -> Vec<String> {
    vec![
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

/// Uygulama yönlendiricisi.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/app.css", get(app_css))
        .route("/api", get(api_index))
        .route("/api/{*path}", get(api))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

fn dash_error(status: StatusCode, msg: &str, unavailable: bool) -> Response {
    let body =
        json!({"schema": DASHBOARD_SCHEMA, "ok": false, "unavailable": unavailable, "error": msg});
    (status, axum::Json(body)).into_response()
}

/// Host denetimi + güvenlik başlıkları (tüm yanıtlar).
async fn guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_ascii_lowercase);
    let mut resp = match host {
        Some(h) if st.allowed_hosts.contains(&h) => next.run(req).await,
        _ => dash_error(
            StatusCode::MISDIRECTED_REQUEST,
            "Host başlığı izinli değil (yalnız localhost; uzaktan erişim için SSH tüneli)",
            false,
        ),
    };
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; \
             img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

fn asset(ct: &'static str, body: &'static str) -> Response {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    (h, body).into_response()
}

async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX_HTML)
}

async fn app_js() -> Response {
    asset("text/javascript; charset=utf-8", APP_JS)
}

async fn app_css() -> Response {
    asset("text/css; charset=utf-8", APP_CSS)
}

async fn not_found() -> Response {
    dash_error(StatusCode::NOT_FOUND, "bulunamadı", false)
}

/// `GET /api`: beyaz liste (panelin "Tüm komutlar" bölümü bunu kullanır).
async fn api_index() -> Response {
    let cmds: Vec<Value> = READ_COMMANDS
        .iter()
        .map(|c| {
            json!({
                "route": c.route,
                "argv": c.argv,
                "param": matches!(c.param, Param::Ident),
                "query": match c.query { QuerySpec::None => json!([]), QuerySpec::Events => json!(["since", "prio", "kind", "limit"]) },
                "milestone": c.milestone,
                "title": c.title,
            })
        })
        .collect();
    axum::Json(json!({"schema": DASHBOARD_SCHEMA, "ok": true, "data": {"commands": cmds}}))
        .into_response()
}

/// `GET /api/<komut>`: doğrula → CLI'yı çalıştır → zarfı aynen döndür.
async fn api(
    State(st): State<Arc<AppState>>,
    Path(path): Path<String>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return dash_error(StatusCode::BAD_REQUEST, "sorgu çözümlenemedi", false);
    };
    let args = match commands::resolve(&path, &query) {
        Ok((_, a)) => a,
        Err(e @ ApiError::NotFound) => {
            return dash_error(StatusCode::NOT_FOUND, &e.to_string(), false);
        }
        Err(e @ ApiError::BadRequest(_)) => {
            return dash_error(StatusCode::BAD_REQUEST, &e.to_string(), false);
        }
    };
    let Ok(_permit) = st.permits.acquire().await else {
        return dash_error(StatusCode::SERVICE_UNAVAILABLE, "sunucu kapanıyor", false);
    };
    match runner::run(&st.cli, &args).await {
        CliOutcome::Envelope(v) => (StatusCode::OK, axum::Json(v)).into_response(),
        CliOutcome::CliError(v) => (StatusCode::BAD_GATEWAY, axum::Json(v)).into_response(),
        CliOutcome::Unavailable(m) => dash_error(StatusCode::NOT_IMPLEMENTED, &m, true),
        CliOutcome::Failed(m) => dash_error(StatusCode::BAD_GATEWAY, &m, false),
    }
}
