# ADR 0012 — Grok fiyat/seans alarmları ve maker/taker ücreti

- Durum: öneri (2026-11-02; Arman onayı ve deploy bekliyor)

## Bağlam
Grok'un notu (2026-11-02): "2 BTC'lik işlemde $456 zararın $156'sı komisyondu. Limit emirler maker mı taker mı
ücretlendiriliyor? İki kontrol arasında fırsatı kaçırmamak için fiyat seviyesi / seans açılışı alarmı eklenebilir mi?"

Ölçüm (VPS journal): giriş 2.000 BTC @ 78050.0 bekleyen limit (09:08'de verildi, 09:26'da doldu) ücret $78,05;
stop çıkışı $77,8999. İkisi de notional'ın %0,05'i: `instruments.toml` tek oran (taker) taşıyordu.

## Karar 1 — maker/taker
- `CostSpec.maker_fee_rate` (`maker_fee_rate_percent`, isteğe bağlı; boşsa eski davranış). BTCUSDT/ETHUSDT:
  Binance USDⓈ-M VIP 0 maker %0,02 / taker %0,05 (resmi destek makalesi 360033544231, 2026-11-02 teyit).
- Kural (`sim::engine`, tek yer; Grok masası ve stratejiler aynı yoldan geçer — kural 3): verildiği anda son
  fiyata göre marketable OLMAYAN limit **giriş** (alış: limit < son, satış: limit > son) dolunca maker. Piyasa,
  stop giriş, marketable limit ve tüm çıkışlar (stop, hedef, zorunlu çıkış) taker. Son fiyat bilinmiyorsa taker.
- Hedef limitleri bilinçli olarak taker (karamsar): genomlar piyasa girişi + hedef limiti kullanır, sonuçları ve
  önceki denemelerle karşılaştırılabilirlik değişmez (test: `crates/gauntlet/tests/maker_fee.rs`). Hedefin maker
  sayılması ayrı karar (OPEN_QUESTIONS #24).
- Risk kapısı ücret tahmini taker kalır (işlem başı risk tavanı karamsar).
- Limit dolumlarında kayma uygulanmaz: BarFill/TradeFill limitten ya da daha iyi (gap'te açılış) doldurur.

## Karar 2 — alarmlar
- CLI `alert add|list|cancel` (Grok'a izinli; riski artırmaz: emir vermez). MCP `atlas_alert_set`, `atlas_alerts`,
  `atlas_alert_cancel`; `atlas-admin` ve `atlas-grok` beyaz listeleri güncellendi; dashboard `alert list`'i gösterir.
- Akış grok_order deseni: `grok_alert_set` (seq = alarm no) / `grok_alert_cancelled` (INFO) → daemon her barda
  denetler → `price_alert`. Durum yalnız journal'dan kurulur (`AlertBook::load`); tetiklenmiş ya da iptal alarm
  yeniden başlatmada etkin sayılmaz. Denetim bar zamanıyla yapılır (kuruluştan sonra kapanan, bitişten önce kapanan
  barlar) → belirlenimci; kesinti sonrası boşluk barlarıyla geç tetiklenebilir (`bar_time` olayda).
- Koşullar: `price_above` (bar yükseği ≥ seviye), `price_below` (bar düşüğü ≤ seviye) — kurarken son fiyatın doğru
  tarafında olmalı; geçerlilik 1..168 sa (varsayılan 24). `session_open`: london = 07:00 UTC (Arman'ın tanımı),
  ny = 09:30 `America/New_York` (DST `core::calendar::local_to_utc`), sonraki hafta içi açılış, +60 dk'da düşer.
  Tek seferlik; en fazla 20 etkin alarm.
- **Öncelik: `price_alert` HIGH.** `ops::policy`: INFO hiç uyandırmaz; MEDIUM sessiz saatte (23:30–09:00 TSİ) 09:00
  özetine düşer; HIGH her zaman uyandırır (dedupe + saatte 6 uyandırma limiti; aşan toplu gelir, düşmez). Alarmın
  amacı Grok'u zamanında uyandırmak; MEDIUM olsaydı gece tetiklenen fiyat alarmı saatler sonra özette gelirdi (amaç
  boşa çıkar). Riskler sınırlı: alarm Grok'un açık eylemiyle kurulur, tek seferliktir ve 20 ile sınırlıdır; dedupe
  anahtarı alarm başına (`price_alert:<id>`). Uyandırma limiti sistem HIGH'larıyla ortak: çok alarm aynı anda
  tetiklenirse sistem alarmları `wake_batch`'e kayabilir (yine teslim edilir). GROK_OPS §4.1/§5: bu olay sistem sorunu
  değildir, Arman'a mesaj gerektirmez.

## Sonuçlar
- Grok'un aynı 2 BTC işleminde giriş ücreti $78,05 → $31,22 olurdu (toplam ücret $155,95 → $109,12).
- Yeni olay türleri (`price_alert`, `grok_alert_set`, `grok_alert_cancelled`) journal'a yazıldıktan sonra eski binary
  journal'ı açamaz (HANDOFF §5): deploy sırası önce binary.
- Spec §5 HIGH listesi bu ADR ile `price_alert` kadar genişler (Grok'un kendi isteğiyle kurulan uyandırma);
  CLAUDE.md'ye işlenmesi Arman'ın kararı.
