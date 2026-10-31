# ADR 0008 — M3 otonomi kararları

- Durum: kabul (M3, 2026-10-31)

## Canlı veri: WebSocket değil REST yoklama
1m barlar için her dakika `klines` yoklaması: son görülen bardan itibaren TÜM kapanmış barlar istenir →
boşluk telafisi kendiliğinden (ders §9.1). Yeni bağımlılık (tungstenite) gerekmez. Kesinti fiyat değildir:
yoklama başarısızsa `FeedStatus::Down` (ders §9.2). L2/WS kaydı M4+ (kitap derinliği feature'ı için).

## Daemon: tek iş parçacığı, dakika ritmi
Tokio yok; blok eden HTTP + uyku. Olaylar önce journal'a, sonra uyandırma politikasına, sonra disk outbox'a.
systemd `Type=notify` + `WatchdogSec=300`; daemon her tick `WATCHDOG=1` gönderir (unix soket, ek crate yok).
Durdurma: `var/daemon.stop` dosyası ya da sinyal (journal append-only olduğu için ani durma güvenli).

## Hızlandırılmış dayanıklılık (replay)
`daemon run --replay-days N`: aynı kod sanal saatle in-sample'ın son N günü üzerinde koşar (holdout'a
dokunmaz). `verify m3` 72 saati böyle koşar: yerel HTTP webhook alıcısı, geçici journal/sicil/outbox,
kendi durdurma dosyası, ajanlar zorla kapalı. Gerçek 72 sa / 7 gün koşusu VPS'te (§7.1).

## Merdiven ve onay
Aşamalar append-only `stages` tablosunda; durum = son satır. Duraklatmayı yalnız duraklatan (ya da Arman)
açar; düşürme duraklatmayı kaldırmaz (asıl duraklatanın sahipliğiyle yeniden yazılır). Onaylar tek
kullanımlık: terfi satırı `onay #id` ile işaretlenir, aynı onay ikinci kez kullanılmaz ve onay mevcut
aşamadan sonra açılmış olmalıdır. `atlas approve` yalnız `ATLAS_APPROVER_KEY` ile (SHA-256'sı
`atlas.toml [approvals] key_sha256`), Grok rolünde CLI düzeyinde de reddedilir.

## Ajanlar
`LlmProvider` (Anthropic Messages, xAI chat completions); fiyat ve günlük bütçe config'te; ikisi de
doldurulmadan çağrı yapılmaz. Çıktı `deny_unknown_fields` + anlamsal doğrulama; enstrüman ve risk sistem
tarafından zorlanır. Eleştirmen onayı olmadan aday kuyruğa girmez. Mühendis yalnız dosyaya öneri yazar.

## Arama
Evrim + TPE benzeri Bayes + kaynak Thompson meta-öğrenmesi; her hızlı tarama sicilde "kapı 0" denemesi.
Bilgi tabanı: yapısal mesafesi < 0.05 olan ölü fikir evrim kaynaklarından yeniden denenmez.

## Sapma
SPRT (H₀ μ=μ₀, H₁ μ=μ₀−0.5σ, α=β=0.05, ≥30 işlem) + aşağı yönlü CUSUM (k=0.5σ, h=5σ). μ₀/σ gauntlet
in-sample'ından (R birimi). CUSUM alarmı da düşürme sebebidir.
