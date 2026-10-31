# Değişiklik günlüğü

## M1 — Temel (2026-10-31, devam)

### Eklendi
- **Workspace**: Rust 2024, `core`, `risk`, `journal`, `data`, `sim` crate'leri ve `atlas` CLI; clippy
  (`unwrap/expect/panic` yasak), rustfmt, GitHub Actions CI.
- **core**: `Price(i64)` tick, `Qty(i32)`, `Money(i64)` mikro-dolar, `UnixNanos`; ondalık→tamsayı ayrıştırma
  (f64 yok); `Instrument` + `instruments.toml` yükleyici (maliyetler uydurulmaz, boş = hata);
  `calendar` (haftalık seans, günlük ara, DST, tatil aralıkları, sonraki kapanış/hafta kapanışı);
  piyasa olayları (bar olayı kapanış anında); sistem olayları (§5 öncelikleri); `Intent`, `Fill`;
  `Strategy` trait'i + `Ctx` (yalnız işlenmiş geçmiş; seans/hafta sonuna kalan süre).
- **risk**: prop config yükleyici (boş alan → yüklenmez, hepsi listelenir); `static/eod_trailing/
  intraday_trailing` taban + kilit; günlük zarar (gün başı bakiye/equity); consistency kontrolü + ileriye
  dönük tavan; kontrat tavanı (mini/mikro ağırlıklı); grup net maruziyet; stop sonrası cooldown; haber
  yasağı; firma saatleri (`no_new_entries_after`, `flat_by`); hafta sonu; kill switch (global/hesap/strateji);
  yalnız-sıkılaştırma politika API'si; LLM bütçe izleyici. `proptest`: hiçbir Intent dizisi tavanları aşamaz.
- **journal**: append-only SQLite (UPDATE/DELETE tetikleyiciyle reddedilir), projeksiyon, checkpoint hash'i,
  `replay` doğrulaması, backup API ile anlık görüntü.
- **data**: `.atb` depolama + `DataVersion` (ADR 0001), manifest hash kontrolü, kalite monitörü (boşluk,
  sıra, yinelenen, OHLC, sıfır hacim, kilitli/çapraz kitap), Binance USDⓈ-M 1m çekici (exchangeInfo ile
  tick/lot doğrulaması), Databento maliyet sorgusu + tavan kapısı, `DataSource` + `StoreSource`.
- **sim**: olay tabanlı motor, `BarFill`/`TradeFill`, OCO, gecikme, maliyet çarpanları, zorunlu çıkışlar
  (kill, flat_by, hafta sonu, zaman), kesinti ≠ fiyat; sentetik veri; M1 doğrulamaları; prop uyum testleri.
- **CLI**: `status`, `health`, `events`, `verify m1`, `journal replay|export|checkpoint`,
  `data list|pull-binance|quote-databento`, `kill`, `kill-reset`, `note`; `--json` (`atlas.cli/1`).

### Doğrulama
- `verify m1`: 18/19. Kırmızı: `prop_config_filled` (Arman, OPEN_QUESTIONS #2).
- Gerçek veri: BTCUSDT 1.050.655 + ETHUSDT 1.050.665 bar; rastgele giriş maliyetsiz ≈ 0 (p=0,42), al-tut birebir.

### Güvenlik (otomatik inceleme sonrası)
- RiskGate: tetiklenmiş stop girişlerinde risk, en kötü dolum fiyatından (`risk_entry_ref`) hesaplanır.
- CLI: `journal export` yalnız `var/exports/<ad>`, üzerine yazmaz; Grok için `--home` yasak;
  kill gerekçesi/not/strateji kimliğinde kontrol karakteri reddedilir, journal metni ekrana arındırılarak basılır.
