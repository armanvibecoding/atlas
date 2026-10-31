# ADR 0003 — M2 doğruluk makinesi kararları

- Durum: kabul (M2, 2026-10-31)

## 1. Feature store: artımlı hesaplayıcı + paylaşılan sütunlar
- Her feature artımlı bir `Calc`'tır (`update(bar) -> Option<f64>`). `FeatureFrame::push` canlıda, aynı
  hesaplayıcılarla önceden hesaplanmış sütunlar backtest'te kullanılır → tek kod yolu (kural 3).
- Önbellek anahtarı: (veri versiyonu, feature, feature sürümü). Gauntlet'te binlerce genom aynı sütunları paylaşır.
- Lookahead dedektörü: kesim noktasından sonraki barlar aynalanır/değiştirilir; öncesindeki değerler bit
  düzeyinde aynı kalmalı. Hileli feature (`Cheater`) testle ve `verify m2`'de yakalanır.
- Kitap derinliği oranı (spec başlangıç seti) **yok**: L2 verisi yok (M3 kripto WS kaydıyla gelecek).
- Asya/Londra pencereleri UTC 00–07 / 07–16 (sabit, yaz saati yok); kripto için yeterli, CME'de gözden geçirilecek.

## 2. Genom DSL
- TOML; `entry.long/short` koşul ağacı (`all/any/not/gt/lt/cross_above/cross_below`), operand = feature
  referansı (`ema:20`, gecikme `@n`), sabit ya da `add/sub/mul`. Stop = `stop_atr × ATR`, hedef R katı, zaman çıkışı.
- Boyutlandırma: `risk_usd / (stop × tick değeri)`, aşağı yuvarlanır.
- Karar bar kapanışında, giriş bir sonraki barın açılışında (piyasa emri).
- Evrim operatörleri: parametre mutasyonu (×U(0.7,1.3), aralığa kırpılır), primitive takası, çaprazlama, budama.

## 3. Gauntlet
- Araştırma koşuları prop kurallarından bağımsız ("research env"); prop kuralları kapı 9'da işlem listesine
  Monte Carlo ile uygulanır. Böylece günlük limit veriyi kesip istatistiği bozmaz.
- Adaylar sabit parametrelidir (arama gauntlet'ten önce olur). CPCV/WF bu yüzden "eğit-test" değil, zaman
  dilimlerinde performans tutarlılığı ölçer; purge = dilim sınırını aşan işlem atılır, embargo = eğitim
  diliminden sonra gelen test diliminin ilk %1'i.
- DSR: V[SR] = max(sicildeki deneme Sharpe'larının varyansı, 1/T); N = sicildeki toplam deneme + bu aday.
- PBO: aday + ±%20 parametre komşuları konfigürasyon kümesi, 8 zaman bloğu, CSCV (70 kombinasyon).
- Rejim: ATR(60)/fiyat medyanın üstü/altı × fiyat EMA(200) üstü/altı = 4 rejim.
- Prop MC: günler yerine konarak örneklenir; gün içi açık-pozisyon dalgalanması görülmez (iyimser yanlılık).
- Holdout: son 90 gün, bütçe 5 erişim; yalnız `HoldoutVault::access` ile (kural 8).
- Sahte strateji testi iki varyantla koşar: gerçek maliyetle ve **sürtünmesiz** (gürültü ücretle otomatik
  ölmesin; istatistik kapıları sınansın).

## 4. Performans
- `RiskState` gün sınırını önbellekler; `StrategyId = Arc<str>`; emir eşleştirme bellek ayırmaz;
  araştırma koşularında pozisyon olay yükleri kurulmaz (`record_position_events = false`; risk olayları ve
  işlem kayıtları aynen). Canlı/paper'da her şey kaydedilir.
- Tek genom (trend_pullback) 921 bin BTC barında ~215 ms → NQ 2 yıl eşdeğeri (717 bin bar) ~167 ms.

## 5. Yeni bağımlılık
- `rayon` (spec listesinde): sahte strateji testi paralel; sonuçlar indeks sırasında toplanır, sicil yazımı
  sıralı → deterministik.

## 6. Depolama formatı v2
- Bar kaydı 72 bayt: `buy_volume` (taker alış hacmi) eklendi (CVD/delta için). v1 dosyaları okunmaz; veri
  yeniden çekildi.
