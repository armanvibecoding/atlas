# ADR 0009 — Grok erişimi: SSH yerine uzak MCP sunucusu

- Durum: kabul (2026-11-01, Arman onayı)

## Bağlam
Spec §6, Grok'un sisteme SSH zorunlu komutuyla (`atlas-admin`) bağlanacağını varsayıyordu. Grok, Arman'ın
uygulamasında çalışan bir asistan: SSH istemcisi yok, ama uzak MCP sunucularını (Streamable HTTP, Bearer/OAuth)
ekleyip araçlarını hem sohbette hem zamanlanmış routine'lerde çağırabiliyor ve webhook ile uyandırılabiliyor
(Grok'un kendi beyanı, 2026-11-01).

## Seçenekler
1. SSH: Grok'un SSH yeteneği yok — uygulanamaz.
2. Salt-okunur durum sayfası: kısıtlayıcı eylemler (pause/kill) yapılamaz, §6 tablosu karşılanmaz.
3. **Uzak MCP sunucusu**: §6 beyaz listesi araç olarak; aynı izin katmanları.

## Karar
`crates/mcp` → `atlas-mcp` ikilisi. Yalnız `POST /mcp` (JSON-RPC; SSE akışı yok), sabit zamanlı Bearer token
(`/etc/atlas/mcp.env`, ≥32 karakter). 16 araç §6 izinli komutlarına birebir; `approve`, `kill-reset`, `risk`,
`deploy`, `gauntlet`, `data`, `daemon`, `webhook-test` araç olarak hiç tanımlı değil.

Savunma katmanları:
1. Araç listesi + sıkı argüman doğrulama (serbest metin `--` ayracından sonra, kontrol karakteri yok).
2. Süreç `grok` kullanıcısı; komutlar `sudo -n -u atlas /usr/local/lib/atlas/atlas-grok` ile — yardımcı
   beyaz listeyi argv düzeyinde yeniden uygular, `ATLAS_ROLE=grok` ve `ATLAS_HOME`'u zorlar.
3. CLI `guard_role` (ATLAS_ROLE=grok).
4. nginx: TLS (Let's Encrypt), yalnız `/mcp`, IP başına hız sınırı, 64 KB gövde sınırı. Sunucu yalnız
   127.0.0.1'i dinler.

Webhook: Grok'un uyandırma adresi Bearer anahtarı ister → `HttpSender::with_auth`; URL ve anahtar
`/etc/atlas/atlas.env`'de (`ATLAS_WEBHOOK_URL`, `ATLAS_WEBHOOK_KEY`; kural 10). `atlas webhook-test` (yalnız Arman)
§7.1 madde 4'ün test olayını gönderir.

## Sonuçlar
- SSH zorunlu komutu (`atlas-admin`) yedek erişim yolu olarak kalır; Grok'a anahtar verilmesi gerekmez.
- MCP servisi sudo kullandığı için `NoNewPrivileges` ve boş yetki kümesi uygulanamaz (diğer sertleştirmeler var).
- `verify` MCP servisinin cgroup'unda koşar (2 GB, 1 çekirdek); aynı anda tek ağır çağrı.
- Token sızarsa: `/etc/atlas/mcp.env` yenilenir, `systemctl restart atlas-mcp`, Grok bağlantısı güncellenir.
