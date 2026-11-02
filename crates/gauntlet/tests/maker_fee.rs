#![allow(clippy::unwrap_used)]
//! Maker/taker ayrımı (instruments.toml `maker_fee_rate_percent`) araştırma sonuçlarını değiştirmez:
//! genomlar piyasa girişi + hedef limiti kullanır; maker oranı yalnız kitapta bekleyip dolan limit
//! GİRİŞLERİNE uygulanır. Beş referans strateji gerçek `configs/instruments.toml` ile, maker oranı
//! varken ve yokken birebir aynı işlem listesini üretmeli.

use atlas_core::{InstrumentTable, Price, UnixNanos};
use atlas_features::FeatureStore;
use atlas_gauntlet::eval::{Cost, Evaluator, Market, always_calendar, research_env};
use atlas_genome::Genome;
use atlas_sim::synthetic::ar_bars;
use std::path::PathBuf;
use std::sync::Arc;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const REFERENCE: &[&str] = &[
    "trend_pullback",
    "sweep_mss",
    "vwap_mean_reversion",
    "opening_range_breakout",
    "cvd_divergence",
];

#[test]
fn referans_genomlar_maker_oranindan_etkilenmez() {
    let text = std::fs::read_to_string(repo().join("configs/instruments.toml")).unwrap();
    let with_maker = InstrumentTable::from_toml_str(&text).unwrap();
    assert!(
        with_maker
            .get("BTCUSDT")
            .unwrap()
            .costs
            .maker_fee_rate
            .is_some(),
        "bu test BTCUSDT'de maker oranı tanımlıyken anlamlı"
    );
    let without: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("maker_fee_rate_percent"))
        .collect::<Vec<_>>()
        .join("\n");
    let no_maker = InstrumentTable::from_toml_str(&without).unwrap();
    assert!(
        no_maker
            .get("BTCUSDT")
            .unwrap()
            .costs
            .maker_fee_rate
            .is_none()
    );

    let cal = always_calendar();
    // ~5 gün sentetik BTC 1m (70000 civarı, hafif momentum → işlem çıksın).
    let bars = Arc::new(ar_bars(
        3,
        7200,
        UnixNanos(1_700_000_000_000_000_000),
        0.15,
        60.0,
        Price(700_000),
    ));
    let market = |t: &InstrumentTable| Market {
        dataset: "sentetik".into(),
        data_version: "maker-test".into(),
        instrument: t.get("BTCUSDT").unwrap().clone(),
        session: cal.session("always").unwrap().clone(),
        bars: Arc::clone(&bars),
    };
    let store = FeatureStore::new();
    let ev_a = Evaluator::new(research_env(&no_maker, &cal).unwrap(), &store);
    let ev_b = Evaluator::new(research_env(&with_maker, &cal).unwrap(), &store);
    let (ma, mb) = (market(&no_maker), market(&with_maker));
    let mut total = 0;
    for name in REFERENCE {
        let g = Genome::from_toml_str(
            &std::fs::read_to_string(repo().join(format!("strategies/{name}.toml"))).unwrap(),
        )
        .unwrap();
        let a = ev_a.run(&g, &ma, Cost::BASE, None).unwrap();
        let b = ev_b.run(&g, &mb, Cost::BASE, None).unwrap();
        assert_eq!(a, b, "{name}: maker oranı sonucu değiştirdi");
        total += a.len();
    }
    assert!(total > 10, "karşılaştırma anlamlı olsun: {total} işlem");
}
