# ADR 0002 — M1 bağımlılık kararları

- Durum: kabul (M1, 2026-10-31)

## Bağlam

Spec §2 bağımlılık listesini verir; listede olmayan her ek için ADR gerekir. Ayrıca bazı listedeki
crate'lerin M1'de kullanılmaması da kayda geçmeli.

## Kararlar

| Crate | Durum | Gerekçe |
|---|---|---|
| `sha2` | **eklendi** | `DataVersion`, journal projeksiyon hash'i, sim sonucu parmak izi (ADR 0001). Saf Rust, küçük. |
| `tempfile` | **eklendi (yalnız dev)** | Testlerde geçici dizin. |
| `rand` 0.9 + `rand_chacha` 0.9 | sürüm sabitlendi | crates.io'da 0.10 var; 0.9 API'si kararlı ve bilinen. Sürüm değişimi rastgele akışı değiştirebilir → determinizm için bilinçli sabitleme. |
| `toml` 0.8, `reqwest` 0.12, `statrs` 0.18, `rusqlite` 0.32 | sürüm sabitlendi | Aynı gerekçe: bilinen API, tekrarlanabilir derleme. Yükseltme ayrı PR + tam test. |
| `chrono-tz` | `serde` özelliği açıldı | `PropRules` serileştirmesi (rapor/JSON). |
| `tokio` | M1'de **kullanılmadı** | M1 CLI ve veri çekimi senkron (`reqwest::blocking`). Daemon/WS M3'te tokio ile gelecek. |
| `databento`, `dbn` | M1'de **eklenmedi** | M0 (Databento hesabı/anahtarı) yok. Maliyet sorgusu + tavan kapısı düz HTTP ile hazır; DBN indirme anahtar gelince eklenecek. |
| `insta`, `criterion`, `rayon`, `wasmtime`, `axum` | ertelendi | Sırasıyla M2 (rapor snapshot, benchmark, paralel arama), M4 (plugin, dashboard). |

## Lint politikası

`[workspace.lints.clippy]`: `unwrap_used`, `expect_used`, `panic`, `todo`, `dbg_macro` = deny; testlerde
`clippy.toml` ile serbest. `unsafe_code = forbid`. CI: `cargo fmt --check`, `cargo clippy -- -D warnings`,
`cargo test`.
