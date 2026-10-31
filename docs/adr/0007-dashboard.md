# ADR 0007 — Dashboard: CLI alt süreci üzerinden salt-okunur web paneli

- Durum: kabul (M4, 2026-10-31)

## Bağlam

Spec §3 `dashboard/` crate'ini "Axum web arayüzü + JSON API" olarak tanımlar; M4 kabulü
"dashboard tüm CLI verisini gösterir" der. CLI (`atlas`, §6) zaten Grok'un tek arayüzüdür, çıktısı
sürümlü bir şemayla (`atlas.cli/1`, `docs/cli-schema.md`) sabittir ve izin kontrolü (`ATLAS_ROLE=grok`)
CLI'nın içindedir.

## Seçenekler

1. **Dashboard crate'leri doğrudan okur** (`journal`, `registry`, `data`…). Hızlı, ama CLI'daki her
   hesap (status özeti, sağlık kontrolleri, strateji aşaması…) ikinci kez yazılır; iki kopya zamanla
   ayrışır, "tüm CLI verisi" garanti edilemez. `bins/atlas` içindeki mantık kütüphaneye taşınmadan
   yapılamaz.
2. **CLI'yı `--json` ile alt süreç olarak çağırmak.** Tek veri kaynağı CLI'dır; şema zaten sabit ve
   testli. CLI'ya eklenen her okuma komutu dashboard'da bir satırlık beyaz liste girişiyle görünür.
   Bedeli: istek başına süreç başlatma (~10–100 ms) — yerel, düşük trafikli bir panel için önemsiz.
3. CLI'yı kütüphaneye bölüp iki ikilinin paylaşması. Doğru yön olabilir ama `bins/atlas` başka
   işlerle eşzamanlı değişiyor; M4 için gereksiz büyük değişiklik.

## Karar

Seçenek 2. `crates/dashboard` (paket `atlas-dashboard`, ikili `atlas-dashboard`):

- `READ_COMMANDS` (`src/commands.rs`) tek beyaz listedir: `status`, `health`, `events`,
  `journal replay`, `data list`, `strategies list|show <ad>`, `experiments summary`, ve M3 için
  şimdiden `report daily|weekly`, `budget`, `approvals list|show <id>`. CLI bir komutu tanımıyorsa
  (clap "unrecognized subcommand") API **501 + `unavailable: true`** döner, panel "henüz yok" yazar.
- `EXCLUDED` bilinçli dışarıda bırakılanları gerekçesiyle listeler: tüm yazma/eylem komutları
  (`kill`, `kill-reset`, `note`, `strategy …`, `approve`, `gauntlet`, `journal export|checkpoint`,
  `data pull-binance|quote-databento|measure-slippage`) ve `verify` (journal'a sonuç olayı yazar,
  dakikalar sürer).
- Kapsam testi (`tests/coverage.rs`): GROK_OPS.md ve CLAUDE.md §6 tablolarındaki her ✓ komut ile
  derlenmiş `atlas --help` ağacındaki her yaprak komut ya `READ_COMMANDS`'ta ya `EXCLUDED`'da olmalı;
  ✗ komutlar beyaz listede olamaz. CLI'ya sınıflandırılmamış komut eklenirse test kırılır.
- API: `GET /api/<komut>` → CLI zarfı aynen (çıkış 0/1 → 200; "sağlıksız", "tutarsız" da veridir).
  CLI hata zarfı (çıkış 2) → 502, gövde yine `atlas.cli/1`. Çalıştırılamama / zaman aşımı / bozuk
  çıktı → 502, gövde `atlas.dashboard/1`. Geçersiz argüman → 400, liste dışı → 404. `GET /api`
  beyaz listeyi verir. `GET /` gömülü tek sayfa panel (HTML/CSS/vanilla JS, harici kaynak yok).

## Güvenlik modeli

- **Yalnız okuma.** Yalnız `GET` yönlendirilir; beyaz listede yazma komutu yoktur (birim testli).
- **İkinci katman:** alt süreç her zaman `ATLAS_ROLE=grok` ile çalışır; CLI'nın kendi izin
  kontrolü (yalnız-Arman komutları, `--home` reddi) yine uygulanır.
- **Kabuk yok:** `std::process::Command` doğrudan argüman listesiyle (tokio sarmalayıcısı üzerinden)
  çalışır. Windows'ta `.bat/.cmd` (cmd.exe argüman yorumu) ikili olarak reddedilir.
- **Sıkı doğrulama:** ad/kimlik `[A-Za-z0-9._:-]`, 1–64 karakter, `-`/`.` ile başlayamaz, `..` yok;
  `events` için `since` (RFC3339 ya da `seq:N`), `prio` (high|medium|info), `kind` (`[a-z_]`),
  `limit` (1–1000); bilinmeyen ya da tekrarlanan parametre 400. Bayraklar `--ad=değer` biçiminde.
- **Ağ:** varsayılan `127.0.0.1:8787`. **Kimlik doğrulama yoktur**; bu yüzden localhost dışı adres
  `--allow-remote` olmadan reddedilir. VPS'te erişim SSH tüneliyle:
  `ssh -L 8787:127.0.0.1:8787 <vps>` → yerelde http://127.0.0.1:8787.
- **DNS rebinding:** `Host` başlığı izinli listede değilse 421 (yalnız localhost varyantları + dinleme
  adresi + açık `--allow-host`).
- **Tarayıcı:** sıkı CSP (`default-src 'none'; script-src 'self'` …, satır içi betik/stil yok),
  `nosniff`, `X-Frame-Options: DENY`, `no-store`. Journal/LLM kaynaklı metin DOM'a yalnız
  `textContent` ile yazılır (`innerHTML` yok).
- **Kaynak:** aynı anda en fazla 4 CLI süreci (semafor), komut başına zaman aşımı (varsayılan 30 sn,
  `kill_on_drop` ile süreç öldürülür), stdout üst sınırı 32 MB.

## Bağımlılıklar

| Crate | Sürüm | Özellikler | Gerekçe |
|---|---|---|---|
| `axum` | 0.8.9 | varsayılanlar kapalı; `http1`, `tokio`, `query`, `json` | spec §2 "Web: axum". HTTP/2, ws, form, tracing gereksiz. |
| `tokio` | 1.54 | `rt-multi-thread`, `macros`, `net`, `process`, `time`, `sync`, `signal` | spec §2 çalışma zamanı; alt süreç + zaman aşımı + Ctrl-C ile düzgün kapanış. |
| `tower` | 0.5.3 | yalnız `util`, **yalnız dev** | testlerde `ServiceExt::oneshot` (port açmadan router çağrısı). |

`clap`, `anyhow`, `serde_json`, `tempfile` mevcut çalışma alanı bağımlılıklarıdır (`anyhow` yalnız
ikilide kullanılır).

## Sonuçlar

- Panel CLI ile aynı veriyi gösterir; CLI'daki hata/izin davranışı panele aynen yansır.
- Olay akışı CLI'nın artan sıralı `events` çıktısını kullanır; panel son 1000 seq penceresini
  (`since=seq:<son-1000>`) çekip en yeniyi üste koyar. Daha eski olaylar "Tüm komutlar" bölümünden
  `since` ile sorgulanır.
- `strategies show <ad>`: CLI adı önce dosya yolu olarak dener (`load_genome`); dashboard ad
  doğrulaması dizin ayırıcıyı yasaklar ve alt sürecin çalışma dizini `ATLAS_HOME`'dur, yani yalnız
  ATLAS_HOME kökündeki dosya adları denenebilir. Bu CLI davranışı ayrıca sıkılaştırılabilir.
- İstek başına süreç maliyeti nedeniyle otomatik yenileme 15 sn'dir; yüksek trafik hedeflenmez.
