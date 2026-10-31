# example_momentum — örnek Atlas plugin'i

20 barlık kanal kırılımı: düzken son kapanış önceki 20 barın en yükseğinin üstündeyse alış, en düşüğünün
altındaysa satış. Stop kanalın karşı ucu, hedef 2R, zaman çıkışı 60 bar, adet 1. Durum tutmaz.

ABI v1 (ayrıntı: `crates/plugin/src/lib.rs`, `docs/adr/0004-plugin-sandbox.md`):

- Dışa aktarılanlar: `memory`, `alloc(len: i32) -> i32`, `on_event(ptr: i32, len: i32) -> i64`
  (dönüş: yüksek 32 bit çıktı adresi, düşük 32 bit uzunluk; 0 = intent yok).
- Girdi: JSON görünüm (`abi`, `instrument`, `now`, `bars[]` OHLCV tick cinsinden, `position`,
  `pending_entry`, `week_close_ns`).
- Çıktı: JSON intent listesi, ör. `[{"action":"open","side":"buy","qty":1,"stop_loss":998,"take_profit":1034,"time_exit_bars":60}]`.
  Diğer eylemler: `{"action":"close"}`, `{"action":"cancel_entry"}`. Bilinmeyen alan = geçersiz çıktı.
- **Hiçbir import kullanılamaz** (WASI, ağ, dosya, saat, rastgelelik yok). Hedef `wasm32-unknown-unknown`
  olmalı; `wasm32-wasip1` hedefi veya `wasm-bindgen` import üretir ve modül yüklenmez.

Bu crate Atlas workspace'inin üyesi değildir (kök `Cargo.toml`: `exclude = ["plugins"]`).

## Derleme

```sh
rustup target add wasm32-unknown-unknown
cd plugins/example_momentum
cargo build --release --target wasm32-unknown-unknown
# çıktı: target/wasm32-unknown-unknown/release/example_momentum.wasm
```

Host testleri derlenmiş kopyayı `crates/plugin/tests/fixtures/example_momentum.wasm` olarak kullanır
(böylece `cargo test` wasm32 hedefi gerektirmez). Kaynak değişirse yeniden derleyip kopyalayın:

```sh
cp target/wasm32-unknown-unknown/release/example_momentum.wasm ../../crates/plugin/tests/fixtures/
```

Kaynaktan derleyip yükleyen test varsayılan olarak koşmaz (hedef + ağ ister):

```sh
cargo test -p atlas-plugin -- --ignored
```

Not: `.wasm` baytları (dolayısıyla plugin hash'i) derleyici sürümüne ve bağımlılık yollarına göre
değişebilir; sicile yazılan hash yüklenen baytların hash'idir, kaynağın değil.
