#!/usr/bin/env bash
# Atlas kurulum betiği (Debian/Ubuntu VPS). Kök olarak çalıştır: sudo bash deploy/install.sh
# Yaptığı: kullanıcı + dizinler, binary/config kopyası, /etc/atlas/atlas.env (600) şablonu,
# systemd birimi, Grok için atlas-admin sarmalayıcısı. Var olan env dosyasının ÜZERİNE YAZMAZ.
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX=/opt/atlas

[[ $EUID -eq 0 ]] || { echo "root gerekli" >&2; exit 1; }
command -v rsync >/dev/null || apt-get install -y rsync
command -v cargo >/dev/null || { echo "önce Rust kur: https://rustup.rs" >&2; exit 1; }

id atlas >/dev/null 2>&1 || useradd --system --home "$PREFIX" --shell /usr/sbin/nologin atlas
id grok >/dev/null 2>&1 || useradd --create-home --shell /bin/bash grok

echo "→ derleme (release)"
(cd "$SRC" && nice -n 15 cargo build --release -j 4 -p atlas -p atlas-mcp)

install -d -m 750 "$PREFIX" "$PREFIX/bin"
install -d -o atlas -g atlas -m 750 "$PREFIX/var" "$PREFIX/var/log"
install -m 755 "$SRC/target/release/atlas" "$PREFIX/bin/atlas"
if [[ -f "$SRC/target/release/atlas-dashboard" ]]; then
  install -m 755 "$SRC/target/release/atlas-dashboard" "$PREFIX/bin/atlas-dashboard"
fi
# Sunucudaki atlas.toml (webhook, onay anahtarı, bütçeler) güncellemede EZİLMEZ; yalnız ilk kurulumda kopyalanır.
rsync -a --delete --exclude atlas.toml "$SRC/configs/" "$PREFIX/configs/"
if [[ -L "$PREFIX/configs/atlas.toml" ]]; then
  echo "HATA: $PREFIX/configs/atlas.toml sembolik bağlantı — reddedildi" >&2; exit 1
fi
if [[ ! -f "$PREFIX/configs/atlas.toml" ]]; then
  install -m 640 -o root -g atlas "$SRC/configs/atlas.toml" "$PREFIX/configs/atlas.toml"
elif ! cmp -s "$SRC/configs/atlas.toml" "$PREFIX/configs/atlas.toml"; then
  echo "NOT: repo atlas.toml sunucudakinden farklı; sunucudaki korundu. Fark: diff $SRC/configs/atlas.toml $PREFIX/configs/atlas.toml"
fi
rsync -a --delete "$SRC/strategies/" "$PREFIX/strategies/"
# deploy/ tamamı (verify m3 sarmalayıcıyı ve kurulum dosyalarını buradan denetler).
rsync -a --delete "$SRC/deploy/" "$PREFIX/deploy/"
# Sahiplik: servis yalnız var/ altına yazar; bin/, configs/, strategies/, deploy/ root'a ait (servis hesabı
# root'un çalıştırdığı/okuduğu dosyaları değiştiremez).
chown root:atlas "$PREFIX"
chmod 750 "$PREFIX"
chown -R root:atlas "$PREFIX/bin" "$PREFIX/configs" "$PREFIX/strategies" "$PREFIX/deploy"
chmod -R go-w "$PREFIX/bin" "$PREFIX/configs" "$PREFIX/strategies" "$PREFIX/deploy"
chmod -R g+rX "$PREFIX/bin" "$PREFIX/configs" "$PREFIX/strategies" "$PREFIX/deploy"
# var/ yalnız servis hesabına ait (chown -R sembolik bağlantıyı takip etmez). Grok buraya doğrudan yazamaz.
chown -R atlas:atlas "$PREFIX/var"
chmod 750 "$PREFIX/var"

install -d -m 750 /etc/atlas
if [[ ! -f /etc/atlas/atlas.env ]]; then
  cat > /etc/atlas/atlas.env <<'ENV'
# Atlas sırları — repo'da DEĞİL. chmod 600.
# ATLAS_ANTHROPIC_KEY=
# ATLAS_XAI_KEY=
# ATLAS_DATABENTO_KEY=
ENV
fi
chmod 600 /etc/atlas/atlas.env
chown root:atlas /etc/atlas/atlas.env

install -m 755 "$SRC/deploy/atlas-admin" /usr/local/bin/atlas-admin
install -m 644 "$SRC/deploy/atlas.service" /etc/systemd/system/atlas.service
install -m 644 "$SRC/deploy/logrotate.conf" /etc/logrotate.d/atlas
# Grok yalnız tek yardımcıyı servis hesabı olarak çalıştırabilir (rol orada zorlanır).
install -d -m 755 /usr/local/lib/atlas
install -m 755 -o root -g root "$SRC/deploy/atlas-grok" /usr/local/lib/atlas/atlas-grok
SUDOERS_TMP="$(mktemp)"
echo "grok ALL=(atlas) NOPASSWD: /usr/local/lib/atlas/atlas-grok" > "$SUDOERS_TMP"
visudo -cf "$SUDOERS_TMP" >/dev/null
install -m 440 -o root -g root "$SUDOERS_TMP" /etc/sudoers.d/atlas-grok
rm -f "$SUDOERS_TMP"
# Eski kurulumdan kalan grup üyeliği kaldırılır.
gpasswd -d grok atlas >/dev/null 2>&1 || true

# MCP sunucusu (Grok'un araçları, ADR 0009): grok kullanıcısı olarak çalışır, /opt/atlas'a erişimi yoktur.
install -m 755 -o root -g root "$SRC/target/release/atlas-mcp" /usr/local/lib/atlas/atlas-mcp
install -m 644 "$SRC/deploy/atlas-mcp.service" /etc/systemd/system/atlas-mcp.service
if [[ ! -f /etc/atlas/mcp.env ]]; then
  umask 077
  echo "ATLAS_MCP_TOKEN=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')" > /etc/atlas/mcp.env
  umask 022
fi
chown root:root /etc/atlas/mcp.env
chmod 600 /etc/atlas/mcp.env

systemctl daemon-reload
systemctl enable atlas.service atlas-mcp.service
systemctl try-restart atlas-mcp.service

cat <<MSG
Kurulum tamam.
1) Veri:   sudo -u atlas ATLAS_HOME=$PREFIX $PREFIX/bin/atlas data pull-binance --symbol BTCUSDT --from 2024-11 --to \$(date +%Y-%m)
2) Kontrol: sudo -u atlas ATLAS_HOME=$PREFIX $PREFIX/bin/atlas verify m1
3) Başlat: systemctl start atlas && journalctl -u atlas -f
4) Grok anahtarı (/home/grok/.ssh/authorized_keys):
   restrict,command="/usr/local/bin/atlas-admin" ssh-ed25519 AAAA... grok
5) Webhook: /etc/atlas/atlas.env → ATLAS_WEBHOOK_URL=… ve ATLAS_WEBHOOK_KEY=… ; sonra: atlas webhook-test
6) MCP: systemctl start atlas-mcp ; token /etc/atlas/mcp.env ; TLS için deploy/nginx-atlas-mcp.conf
MSG
