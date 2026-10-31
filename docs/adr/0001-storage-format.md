# ADR 0001 — Bar depolama formatı ve veri versiyonu

- Durum: kabul (M1, 2026-10-31)
- Karar veren: Claude Code (Arman'ın onayına açık)

## Bağlam

Spec §2: "Bar/feature depolama formatı M1'de ADR ile seçilir (DBN dosyaları + kendi kolon formatı ya da Parquet)."
Gereksinimler:

1. **Determinizm** (kural 2): aynı içerik → aynı `DataVersion`, platformdan bağımsız.
2. **Hız**: 2 yıl NQ 1m (~700k bar) tek genom değerlendirmesi < 200 ms (M2 hedefi) → yükleme ihmal edilebilir olmalı.
3. **Ücretsiz kaynaklar** (Arman kararı, 2026-10-31): Binance, ileride Dukascopy; Databento yalnız ücretsiz krediyle. Format kaynaktan bağımsız olmalı.
4. **Bozulma tespiti**: dosya değişirse sessizce kullanılmamalı.

## Seçenekler

| Seçenek | Artı | Eksi |
|---|---|---|
| Parquet (`arrow`/`parquet`) | sütunlu, sıkıştırma, araç desteği | ağır bağımlılık ağacı, yazım deterministik değil (meta veri, sıkıştırma sürümü) |
| DBN dosyaları (her kaynak için) | Databento yerel formatı | Binance/Dukascopy için DBN üretmek yapay; `dbn` encoder API'sine bağımlılık |
| **Kendi sabit genişlikli formatı** | sıfır bağımlılık, bit düzeyinde deterministik, okuma = memcpy hızında | araç desteği yok (gerekirse dışa aktarım komutu eklenir) |

## Karar

Kendi formatı: `.atb` (Atlas bar), **ay başına bir dosya**, kaynak/sembol/şema dizininde:

```
var/data/<kaynak>/<sembol>/<şema>/<YYYY-AA>.atb
var/data/manifest.json
```

Dosya düzeni (little-endian):

```
"ATLASBAR" (8) | format sürümü u32 (=1) | başlık uzunluğu u32 | başlık JSON | kayıtlar
kayıt (64 bayt): open_ts u64 | close_ts u64 | open i64 | high i64 | low i64 | close i64 | volume u64 (lot) | trades u32 | pad u32
```

- Fiyatlar **tick cinsinden** (`Price(i64)`, kural 1); hacim lot cinsinden. Ondalık → tick dönüşümü çekimde
  tam yapılır; tick'in katı olmayan fiyat hata verir.
- `DataVersion = SHA-256(şema | sembol | tick | lot | aralık | kayıt baytları)`. Başlıktaki çekim zamanı gibi
  meta veriler hash'e **girmez**: aynı içerik yeniden çekilse de aynı versiyon.
- Birden çok ayın versiyonu sıralı birleştirilir (`DataVersion::combine`). Deney kayıtları bu hash'i taşır.
- Okumada her dosyanın içerik hash'i manifestle karşılaştırılır; uyuşmazlık = hata (sessiz kullanım yok).
- Yazım atomik: geçici dosya + `rename`.
- Databento'dan gelecek ham DBN dosyaları (`var/data/raw/`) olduğu gibi saklanır ve **bu formata
  normalize edilir**; arşiv DBN, çalışma formatı `.atb`.

## Sonuçlar

- Yeni bağımlılık: `sha2` (hash). Parquet/arrow yok.
- 1.05M BTCUSDT barı (2 yıl) ≈ 67 MB; tamamı belleğe < 1 sn'de yüklenir; `verify m1` gerçek veri testleri
  dahil 1,7 sn.
- Trades/MBO şemaları aynı düzenle ayrı kayıt tipleriyle eklenecek (M3/M4), format sürümü artırılarak.
- Dışarıdan araçla incelemek gerekirse `atlas data export` (CSV) eklenebilir.
