# ADR 0005 — ML: meta-labeling (`ml` crate)

- Durum: kabul (M4, 2026-10-31)

## Bağlam
Spec §4.3: birincil sinyal (genom/plugin) aday üretir; ikincil model "gir/girme + boyut" verir;
etiketleme triple-barrier; kütüphane seçimi M4'te ADR ile. Değişmezler: determinizm (kural 2),
lookahead imkânsız (kural 4), fiyat tick/para mikro-dolar (kural 1), kütüphanede panik yok.

## Seçenekler
1. Harici ML kütüphanesi (`linfa`, `smartcore`, gradient boosting bağlamaları, ONNX çalışma zamanı).
2. Kendi küçük implementasyonumuz: L2 lojistik regresyon + purged k-fold.

## Karar: harici kütüphane yok, kendi L2 lojistik regresyonumuz
- **Determinizm:** harici kütüphanelerin çoğu iç rastgelelik (başlatma, alt örnekleme, paralel
  toplama sırası) kullanır; bit düzeyinde tekrar üretilebilirliği denetlemek zordur. Bizim
  implementasyonda rastgelelik yok, toplama sırası sabit; aynı veri → aynı katsayılar (test edilir).
- **Bağımlılık:** yeni crate eklenmedi (`serde`, `serde_json`, `thiserror` ve workspace crate'leri).
  `linfa`/`smartcore` geniş bağımlılık ağacı ve `ndarray`/BLAS getirir; derleme ve tedarik zinciri
  yüzeyi büyür.
- **Açıklanabilirlik:** standartlaştırılmış katsayılar doğrudan okunur (hangi özellik hangi yönde,
  ne kadar). Gauntlet'in amacı sahte keşfi öldürmek; az parametreli, aşırı uyumu sınırlı model bu
  amaca uygundur. Daha esnek model (ağaç topluluğu) ancak lojistik model CV'de anlamlı ve kararlı
  edge gösterdikten sonra, ayrı ADR ile düşünülür.

## Etiketleme (triple-barrier)
- Giriş referansı karar barının **kapanışı**; yol sonraki barların yüksek/düşükleriyle taranır.
- Hedef önce +1, stop önce −1, zaman bariyeri 0. **Aynı barda ikisi → −1** (simülatörün
  "aynı mumda stop+hedef → stop" kuralıyla tutarlı). Boşlukla bariyer aşılırsa çıkış açılıştan.
- Veri zaman bariyerinden önce biter ve bariyer dokunulmamışsa etiket bilinmez → örnek atılır
  (yanlı "0" yazılmaz).
- Meta hedefi ikilidir: `y = 1` ⇔ etiket +1. 0 ve −1 "girme" sınıfıdır.
- Bariyerler tick (`i64`); f64 yalnız özellik/istatistik.

## Lookahead
- Etiket geleceği kullanır, yalnız eğitimde. Özellikler `PitColumns::row_at(t0)` ile okunur:
  `available_at ≤ t0` olan son satır (`t0` = karar barının kapanışı). Canlı süzmede aynı fonksiyon
  `ctx.now()` ile çağrılır → eğitim/canlı tek kod yolu. Gecikmeli kullanılabilen feature testte
  bir önceki satıra düştüğü doğrulanır.

## Model
- L2 düzenlileştirilmiş lojistik regresyon, Newton/IRLS; her adım Cholesky ile çözülür, amaç
  iyileşmezse adım yarıya indirilir. Kesişim cezasız; λ toplam log-olabilirliğe göre (varsayılan 1).
- Özellikler eğitim verisinin ortalama/std'siyle standartlaştırılır (std ≈ 0 → 1). Parametreler
  modelle saklanır.
- `Transform::SideSigned`: özellikler yön işaretiyle çarpılır (yönle uyumlu momentum vb.). Dönüşüm
  modelin parçasıdır.
- Serileştirme: JSON, `version` alanı (`MODEL_VERSION = 1`, uyuşmazsa okuma reddedilir), özellik
  adları, eğitim özeti (n, pozitif oranı, zaman aralığı, CV metrikleri). Katsayılar JSON'da **metin**
  olarak yazılır (en kısa gidiş-dönüş biçimi + doğru yuvarlamalı `str::parse`) → okunan model bit
  düzeyinde aynı; serde_json'un varsayılan sayı ayrıştırıcısı son bitte sapabiliyor (ölçüldü).

## Doğrulama: purged + embargo'lu k-fold
- Örnekler `t0`'a göre sıralı, katlar ardışık bloklar (karıştırma yok).
- Test aralığı `[ilk t0, en geç t1]`. Bilgi penceresi `[t0, t1]` bu aralıkla kesişen eğitim örneği
  atılır (purge; testten önce başlayıp içeri taşanlar dahil). Test bitişinden sonraki `embargo_ns`
  içinde başlayan eğitim örnekleri de atılır (embargo).
- Çıktı: havuzlanmış kat dışı AUC ve log-loss, kat başı AUC/log-loss/örnek sayıları/atılanlar.
- Sentetik ölçüm (testlerde, seed sabit): AR(1) φ=0,5 barlarda yönle çarpılmış son getiri →
  CV AUC 0,682 (katlar 0,66–0,71); saf gürültü özellikler → AUC 0,486.

## Karar ve boyutlandırma
- Giriş: `p ≥ eşik` (varsayılan 0,55) **ve** boyut çarpanı `m > 0`.
- `m` varsayılan `clamp(2p − 1, 0, 1)` = kazanç/kayıp oranı 1 olan bahsin tam Kelly kesri.
  Seçenek: kesirli Kelly `m = clamp(kesir · (p − (1−p)/b), 0, 1)`; ya da sabit (yalnız filtre).
- `m ≤ 1`: meta model birincil adedi **yalnız küçültür**, asla büyütmez. Adet `max(1, ⌊adet·m⌋)`;
  girilmiyorsa niyet atılır. Risk kapısı her durumda ayrıca uygulanır (kural 5).

## `MetaFiltered<S: Strategy>`
- Birincil stratejiyi sarar; yalnız `Open` niyetlerini süzer, `Close`/`CancelEntry` dokunulmadan geçer.
- Kimlik birincil stratejininkidir (motor pozisyonu niyetteki kimlikle izler).
- İki kaynak: önceden hesaplanmış sütunlar (backtest, `FeatureStore` → `PitColumns::from_shared`) ya da
  canlıda barlar geldikçe büyüyen `FeatureFrame` (aynı hesaplayıcılar). Sütun adları model adlarıyla
  aynı sırada olmalı, değilse kurulum hata verir.
- Özellik okunamazsa (ısınma, yapılandırılmamış enstrüman) niyet atılır; sayaçlar `MetaStats`'ta.

## Sonuçlar / sınırlamalar
- Doğrusal model: özellik etkileşimlerini kendisi öğrenmez; gerekiyorsa özellik olarak verilmeli.
- Bit determinizmi aynı platform/derleyici için garanti; `exp`/`ln` platform libm'ine bağlı olduğundan
  farklı işletim sistemleri arasında son bit farkı olabilir.
- Purged k-fold tek yol üretir (CPCV'nin çoklu yolları yok); meta modelin kendisi gauntlet'ten
  ayrıca geçmelidir (sarmalanmış strateji yeni bir deney olarak sicile yazılır, kural 7).
- Örnek ağırlıklandırma (eşzamanlılık/benzersizlik ağırlıkları) yok; örtüşen etiketler CV'de purge
  ile ele alınır ama eğitimde eşit ağırlıklıdır.
- `MetaFiltered` tek enstrümanlıdır; başka enstrümanın `Open` niyeti atılır.
