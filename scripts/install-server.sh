#!/bin/sh
# install-server.sh [xi-vault binary]: a Linux machine (a VM at home, say) serves game versions to
# players' launchers. Run as root. It puts xi-vault in /opt/xi-vault (and on PATH, for the update
# publisher's `xi-vault apply` over SSH), the site in /srv/xi-vault/site, a sandboxed systemd
# service on port 54080, and opens that port in ufw or firewalld when one is on.
#
# Then: forward TCP 54080 on the router to this machine, point update.<your server> at your home
# address (DNS), and publish a version into the site (vault/README.md).
set -eu
bin=${1:-./xi-vault}
port=${XI_VAULT_PORT:-54080}
[ "$(id -u)" = 0 ] || { echo "run as root (sudo $0)"; exit 1; }
[ -x "$bin" ] || { echo "no xi-vault binary at $bin (give its path)"; exit 1; }

mkdir -p /opt/xi-vault /srv/xi-vault/site
install -m 755 "$bin" /opt/xi-vault/xi-vault.new
mv /opt/xi-vault/xi-vault.new /opt/xi-vault/xi-vault
ln -sf /opt/xi-vault/xi-vault /usr/local/bin/xi-vault
[ -f /srv/xi-vault/site/index.json ] || echo '{"format":"xi-vault/1","current":"","versions":[],"packs":[]}' > /srv/xi-vault/site/index.json
chmod -R a+rX /srv/xi-vault

cat > /etc/systemd/system/xi-vault.service <<EOF
[Unit]
Description=FINAL FANTASY XI client versions for launchers and updaters (xi-vault)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/opt/xi-vault/xi-vault serve /srv/xi-vault/site --listen 0.0.0.0:$port
DynamicUser=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
NoNewPrivileges=yes
ReadOnlyPaths=/srv/xi-vault
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable xi-vault >/dev/null 2>&1
systemctl restart xi-vault

if command -v ufw >/dev/null && ufw status | grep -q "Status: active"; then
  ufw allow "$port/tcp" comment "xi-vault" >/dev/null
elif command -v firewall-cmd >/dev/null && firewall-cmd --state >/dev/null 2>&1; then
  firewall-cmd --permanent --add-port="$port/tcp" >/dev/null && firewall-cmd --reload >/dev/null
fi

sleep 1
systemctl is-active --quiet xi-vault || { journalctl -u xi-vault -n 20 --no-pager; exit 1; }
echo "xi-vault serves /srv/xi-vault/site on port $port."
ip -4 -o addr show scope global 2>/dev/null | awk '{print "  this machine: http://" substr($4, 1, index($4, "/") - 1) ":'"$port"'/index.json"}'
