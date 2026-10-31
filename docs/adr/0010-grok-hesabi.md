# ADR 0010 — Grok'un ayrı sanal işlem hesabı

- Durum: kabul (2026-11-01, Arman kararı)

## Bağlam
Spec §6, Grok'un riski artıramayacağını söyler (yalnız kısıtlayıcı eylem). Arman 2026-11-01'de Grok'un "istediği
gibi işlem açabilmesini" istedi ve şunları seçti: **ayrı Grok hesabı**, **stop zorunlu, boyut Grok'ta** (işlem
başı risk tavanı ve prop limitleri geçerli), **hemen**.

## Karar
- Grok'a ayrı bir sanal (paper) hesap: `atlas_exec::manual::ManualAccount`, strateji kimliği `grok`. Sistemin
  terfi merdiveni, stratejileri ve onay yolu değişmez; Grok onlara hâlâ yalnız kısıtlayıcı eylem yapabilir.
- Aynı kod yolu (kural 3): Grok emri → `ManualStrategy` → `Intent` → prop `RiskGate` (işlem başı $1000 tavanı,
  kontrat tavanı, günlük zarar, taban, consistency, cooldown) → `BarFill`. Gerçek para yok (M5'e kadar).
- Emir akışı journal'dan geçer: CLI `grok_order` (istek; seq = emir no) → daemon sonraki tick'in son barında
  uygular ve `grok_order_applied {request_seq, bar_ts, order}` yazar. Yeniden başlatmada hesap depodaki barlar +
  bu kayıtlarla birebir yeniden kurulur (belirlenimci).
- CLI: `trade open|close|cancel|list`, `market`. MCP: `atlas_trade_open/close/cancel`, `atlas_trades`,
  `atlas_market`. Üç izin katmanı (MCP araç listesi, `atlas-grok`, `atlas-admin`) güncellendi.
- Kill: global/account kill Grok hesabına da uygulanır; `strategy:grok` yalnız Grok hesabını durdurur. Açmak
  (`kill-reset`) yalnız Arman.
- Daemon her tick `var/grok_account.json` ve `var/market.json` (son 24 sa 1m bar) yazar.

## Sonuçlar
- Grok'un isabetini sistemden bağımsız ölçebiliriz (hesap PnL'i, kazanma oranı).
- Spec §6'daki "Grok riski artıramaz" cümlesi Grok'un **kendi hesabı dışında** geçerlidir.
- Gerçek para M5'te gelirse Grok hesabının gerçek borsaya bağlanması ayrı bir Arman kararıdır (bu ADR kapsamaz).
- Sınır: tek sembol (daemon'un canlı akışı, BTCUSDT), hesapta aynı anda tek pozisyon.
