//! Router testleri: ağ portu açılmaz, `tower::ServiceExt::oneshot` ile çağrılır.
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use atlas_dashboard::{AppState, CliConfig, loopback_hosts, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

const HOST: &str = "127.0.0.1:8787";

fn app(bin: PathBuf, home: Option<PathBuf>) -> axum::Router {
    router(Arc::new(AppState::new(
        CliConfig::new(bin, home),
        loopback_hosts(8787),
    )))
}

/// İkili gerektirmeyen testler için: çağrılırsa hata verecek bir yol.
fn app_no_bin() -> axum::Router {
    app(PathBuf::from("atlas-bu-ikili-yok-xyz"), None)
}

async fn get(app: axum::Router, uri: &str, host: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder().uri(uri);
    if let Some(h) = host {
        b = b.header(header::HOST, h);
    }
    let resp = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let st = resp.status();
    (
        st,
        to_bytes(resp.into_body(), 64 << 20).await.unwrap().to_vec(),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let (st, b) = get(app, uri, Some(HOST)).await;
    let v: Value = serde_json::from_slice(&b)
        .unwrap_or_else(|e| panic!("{uri}: JSON değil ({e}): {}", String::from_utf8_lossy(&b)));
    (st, v)
}

#[tokio::test]
async fn yazma_komutlari_404() {
    for uri in [
        "/api/kill",
        "/api/kill/global",
        "/api/kill-reset/global",
        "/api/note",
        "/api/approve/x",
        "/api/gauntlet/run/trend_pullback",
        "/api/journal/export/x",
        "/api/journal/checkpoint",
        "/api/data/pull-binance",
        "/api/verify/m1",
        "/api/strategy/pause/x",
        "/bilinmeyen",
    ] {
        let (st, v) = get_json(app_no_bin(), uri).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(v["schema"], "atlas.dashboard/1");
        assert_eq!(v["ok"], false);
    }
}

#[tokio::test]
async fn kotu_arguman_400() {
    for uri in [
        "/api/strategies/show/a%3Brm",
        "/api/strategies/show/-x",
        "/api/strategies/show/..",
        "/api/strategies/show/a%20b",
        "/api/strategies/show/%24%28id%29",
        "/api/events?prio=critical",
        "/api/events?limit=0",
        "/api/events?since=--home",
        "/api/events?home=/etc",
        "/api/status?x=1",
    ] {
        let (st, v) = get_json(app_no_bin(), uri).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{uri}: {v}");
    }
    let long = format!("/api/strategies/show/{}", "a".repeat(200));
    assert_eq!(
        get_json(app_no_bin(), &long).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn host_denetimi_ve_guvenlik_basliklari() {
    let (st, _) = get(app_no_bin(), "/", Some("kotu.example:8787")).await;
    assert_eq!(st, StatusCode::MISDIRECTED_REQUEST);
    let (st, _) = get(app_no_bin(), "/", None).await;
    assert_eq!(st, StatusCode::MISDIRECTED_REQUEST);

    let resp = app_no_bin()
        .oneshot(
            Request::builder()
                .uri("/")
                .header(header::HOST, "localhost:8787")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let csp = resp.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap();
    assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'"));
    let html =
        String::from_utf8(to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    assert!(html.contains("Atlas") && html.contains("/app.js"));
    // Harici CDN yok.
    assert!(!html.contains("http://") && !html.contains("https://"));
}

#[tokio::test]
async fn yalniz_get() {
    let resp = app_no_bin()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/status")
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn ikili_yoksa_502() {
    let (st, v) = get_json(app_no_bin(), "/api/status").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY);
    assert!(v["error"].as_str().unwrap().contains("çalıştırılamadı"));
}

#[tokio::test]
async fn api_dizini_beyaz_listeyi_verir() {
    let (st, v) = get_json(app_no_bin(), "/api").await;
    assert_eq!(st, StatusCode::OK);
    let routes: Vec<&str> = v["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["route"].as_str().unwrap())
        .collect();
    assert_eq!(routes.len(), atlas_dashboard::READ_COMMANDS.len());
    assert!(routes.contains(&"status") && routes.contains(&"experiments/summary"));
}

/// Gerçek `atlas` ikilisiyle uçtan uca: geçici ATLAS_HOME, `atlas.cli/1` zarfı.
#[tokio::test]
async fn gercek_cli_ile_status_health_ve_digerleri() {
    let Some(bin) = common::atlas_bin() else {
        return;
    };
    let home = common::temp_home();
    let mk = || app(bin.clone(), Some(home.path().to_path_buf()));

    for (uri, command) in [
        ("/api/status", "status"),
        ("/api/health", "health"),
        ("/api/events?limit=5&prio=info", "events"),
        ("/api/journal/replay", "journal.replay"),
        ("/api/data/list", "data.list"),
        ("/api/strategies/list", "strategies.list"),
        ("/api/strategies/show/trend_pullback", "strategies.show"),
        ("/api/experiments/summary", "experiments.summary"),
    ] {
        let (st, v) = get_json(mk(), uri).await;
        assert_eq!(st, StatusCode::OK, "{uri}: {v}");
        assert_eq!(v["schema"], "atlas.cli/1", "{uri}");
        assert_eq!(v["command"], command, "{uri}");
        assert!(v.get("data").is_some(), "{uri}");
    }
    let (_, s) = get_json(mk(), "/api/status").await;
    assert!(s["data"]["journal"].is_object() && s["data"]["kills"].is_object());
    let (_, h) = get_json(mk(), "/api/health").await;
    assert!(
        h["data"]["checks"]
            .as_array()
            .is_some_and(|c| !c.is_empty())
    );

    // Olmayan strateji: CLI hata zarfı (çıkış 2) → 502, gövde yine atlas.cli/1.
    let (st, v) = get_json(mk(), "/api/strategies/show/bu_strateji_yok").await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["schema"], "atlas.cli/1");
    assert_eq!(v["ok"], false);

    // M3 komutları: CLI tanımıyorsa 501 + unavailable (panel "henüz yok" der); tanıyorsa zarf.
    for uri in ["/api/budget", "/api/report/daily", "/api/approvals/list"] {
        let (st, v) = get_json(mk(), uri).await;
        match st {
            StatusCode::NOT_IMPLEMENTED => assert_eq!(v["unavailable"], true, "{uri}"),
            StatusCode::OK | StatusCode::BAD_GATEWAY => {
                assert_eq!(v["schema"], "atlas.cli/1", "{uri}")
            }
            other => panic!("{uri}: beklenmeyen {other}: {v}"),
        }
    }
}
