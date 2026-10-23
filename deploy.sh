#!/usr/bin/env bash
# Deploy Lanes onto the phext.io host and proxy mc.phext.io at Apache.
# Idempotent: run it again to rebuild exo and restart. Chat data lives in
# DATA_DIR, outside the git checkout, and is seeded only when missing.
#
#   curl -fsSL https://raw.githubusercontent.com/wbic16/phext-lanes/exo/deploy.sh | bash
#   bash deploy.sh [--no-tls] [--branch exo] [--port 7841] [--domain mc.phext.io]
#
# Flags override the same-named environment variables.
# mc.phext.io's A record is 44.248.235.76. Certbot still waits until DNS
# for DOMAIN resolves to this machine's public IP.
set -euo pipefail

BRANCH="${BRANCH:-exo}"
PORT="${PORT:-7841}"
DOMAIN="${DOMAIN:-mc.phext.io}"
EXPECTED_A="${EXPECTED_A:-44.248.235.76}"
APP_DIR="${APP_DIR:-${HOME}/phext-lanes}"
DATA_DIR="${DATA_DIR:-/var/lib/phext-lanes}"
NO_TLS="${NO_TLS:-0}"

REPO="https://github.com/wbic16/phext-lanes.git"
SERVICE="phext-lanes.service"
UNIT_PATH="/etc/systemd/system/${SERVICE}"

usage() {
  cat <<'EOF'
Usage: deploy.sh [--branch NAME] [--port PORT] [--domain NAME]
                 [--app-dir DIR] [--data-dir DIR] [--no-tls]

  BRANCH    git branch to deploy (default exo)
  PORT      localhost port (default 7841)
  DOMAIN    public name (default mc.phext.io)
  APP_DIR   checkout (default ~/phext-lanes)
  DATA_DIR  phext directory kept across redeploys (default /var/lib/phext-lanes)
  --no-tls  do not run certbot

Run as ec2-user. The script uses sudo for yum, systemd, and Apache.
CERTBOT_EMAIL is optional; without it certbot registers without an email.
EOF
}

expand_tilde() {
  local path=$1
  local twiddle home_slash
  twiddle='~'
  home_slash="${twiddle}/"
  if [[ "$path" == "$twiddle" ]]; then
    printf '%s\n' "$HOME"
  elif [[ "$path" == "${home_slash}"* ]]; then
    printf '%s\n' "${HOME}/${path:2}"
  else
    printf '%s\n' "$path"
  fi
}

die() {
  printf 'deploy.sh: %s\n' "$*" >&2
  exit 1
}

# 0 when ss output shows a listener on this port that is not the lanes binary.
port_is_foreign() {
  local dump=$1
  local port=$2
  local line
  [[ -n "$dump" ]] || return 1
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    if ! printf '%s\n' "$line" | grep -Eq ":${port}([^0-9]|$)"; then
      continue
    fi
    if [[ "$line" == *'("lanes"'* ]]; then
      continue
    fi
    return 0
  done <<<"$dump"
  return 1
}

vhost_matches() {
  local file=$1
  local domain=$2
  local port=$3
  [[ -f "$file" ]] || return 1
  local -a reader
  if [[ -r "$file" ]]; then
    reader=(grep -qF)
  else
    reader=(sudo grep -qF)
  fi
  "${reader[@]}" "ServerName ${domain}" "$file" \
    && "${reader[@]}" "ProxyPass / http://127.0.0.1:${port}/" "$file"
}

ss_listen_dump() {
  local port=$1
  local ss_bin=""
  if command -v ss >/dev/null 2>&1; then
    ss_bin=$(command -v ss)
  elif [[ -x /usr/sbin/ss ]]; then
    ss_bin=/usr/sbin/ss
  else
    die "ss is not installed (iproute). Cannot check whether port ${port} is free."
  fi
  local dump
  dump=$(sudo "$ss_bin" -H -ltnp "sport = :${port}" 2>/dev/null || true)
  if [[ -z "$dump" ]]; then
    dump=$(sudo "$ss_bin" -H -ltnp 2>/dev/null | grep -E ":${port}([^0-9]|$)" || true)
  fi
  printf '%s\n' "$dump"
}

public_ipv4() {
  local token="" ip=""
  token=$(curl -fsS --max-time 2 -X PUT \
    -H "X-aws-ec2-metadata-token-ttl-seconds: 60" \
    "http://169.254.169.254/latest/api/token" 2>/dev/null || true)
  if [[ -n "$token" ]]; then
    ip=$(curl -fsS --max-time 2 -H "X-aws-ec2-metadata-token: ${token}" \
      "http://169.254.169.254/latest/meta-data/public-ipv4" 2>/dev/null || true)
  fi
  if [[ -z "$ip" ]]; then
    ip=$(curl -fsS --max-time 2 \
      "http://169.254.169.254/latest/meta-data/public-ipv4" 2>/dev/null || true)
  fi
  if [[ -z "$ip" ]]; then
    ip=$(curl -fsS --max-time 5 "https://checkip.amazonaws.com" 2>/dev/null | tr -d '[:space:]' || true)
  fi
  printf '%s\n' "$ip"
}

domain_v4s() {
  local name=$1
  if command -v getent >/dev/null 2>&1; then
    getent ahosts "$name" 2>/dev/null | awk '$1 !~ /:/ { print $1 }' | sort -u
    return 0
  fi
  if command -v dig >/dev/null 2>&1; then
    dig +short A "$name" 2>/dev/null | grep -E '^[0-9.]+$' || true
  fi
}

dns_points_at() {
  local public=$1
  local name=$2
  local ip
  [[ -n "$public" ]] || return 1
  while IFS= read -r ip; do
    [[ "$ip" == "$public" ]] && return 0
  done < <(domain_v4s "$name")
  return 1
}

http_code() {
  local url=$1
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 20 --max-redirs 5 -L "$url" 2>/dev/null || true)
  if [[ -z "$code" ]]; then
    code=000
  fi
  printf '%s\n' "$code"
}

certbot_print() {
  if [[ -n "${CERTBOT_EMAIL:-}" ]]; then
    printf 'sudo certbot --apache -d %s --non-interactive --agree-tos --redirect --email %s\n' \
      "$DOMAIN" "$CERTBOT_EMAIL"
  else
    printf 'sudo certbot --apache -d %s --non-interactive --agree-tos --redirect --register-unsafely-without-email\n' \
      "$DOMAIN"
  fi
}

restore_file() {
  local dest=$1
  local backup=$2
  local created=$3
  if [[ "$created" != "1" ]]; then
    return 0
  fi
  if [[ -n "$backup" ]]; then
    sudo cp -a "$backup" "$dest"
    printf 'Restored %s from %s\n' "$dest" "$backup" >&2
  else
    sudo rm -f "$dest"
    printf 'Removed %s\n' "$dest" >&2
  fi
}

main() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --branch)
        [[ $# -ge 2 ]] || die "--branch needs a value"
        BRANCH=$2
        shift 2
        ;;
      --port)
        [[ $# -ge 2 ]] || die "--port needs a value"
        PORT=$2
        shift 2
        ;;
      --domain)
        [[ $# -ge 2 ]] || die "--domain needs a value"
        DOMAIN=$2
        shift 2
        ;;
      --app-dir)
        [[ $# -ge 2 ]] || die "--app-dir needs a value"
        APP_DIR=$2
        shift 2
        ;;
      --data-dir)
        [[ $# -ge 2 ]] || die "--data-dir needs a value"
        DATA_DIR=$2
        shift 2
        ;;
      --no-tls)
        NO_TLS=1
        shift
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      *)
        die "unknown argument: $1 (try --help)"
        ;;
    esac
  done

  APP_DIR=$(expand_tilde "$APP_DIR")
  DATA_DIR=$(expand_tilde "$DATA_DIR")

  if [[ "$(id -un)" != "ec2-user" ]]; then
    die "run as ec2-user (this shell is $(id -un)). sudo is used only for system changes."
  fi
  if [[ "${EUID}" -eq 0 ]]; then
    die "do not run as root."
  fi

  if [[ ! "$PORT" =~ ^[0-9]+$ ]] || ((PORT < 1 || PORT > 65535)); then
    die "PORT must be 1-65535 (got ${PORT}). Refusing to continue."
  fi
  if [[ ! "$DOMAIN" =~ ^[A-Za-z0-9.-]+$ ]]; then
    die "DOMAIN must be a hostname (got ${DOMAIN})."
  fi
  case "${APP_DIR}${DATA_DIR}${BRANCH}" in
    *[[:space:]]*)
      die "BRANCH, APP_DIR, and DATA_DIR must not contain spaces."
      ;;
  esac

  sudo -v

  local packages=()
  command -v git >/dev/null 2>&1 || packages+=(git)
  command -v gcc >/dev/null 2>&1 || packages+=(gcc)
  command -v curl >/dev/null 2>&1 || packages+=(curl)
  if [[ ${#packages[@]} -gt 0 ]]; then
    sudo yum install -y "${packages[@]}"
  fi

  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain stable --profile minimal
  fi
  if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
  fi
  command -v cargo >/dev/null 2>&1 || die "cargo is missing after rustup."

  if [[ -e "$APP_DIR" && ! -d "$APP_DIR/.git" ]]; then
    die "${APP_DIR} exists and is not a git checkout."
  fi
  if [[ ! -d "$APP_DIR/.git" ]]; then
    git clone --branch "$BRANCH" "$REPO" "$APP_DIR"
  fi
  git -C "$APP_DIR" fetch origin "$BRANCH"
  git -C "$APP_DIR" checkout -B "$BRANCH" "origin/${BRANCH}"
  git -C "$APP_DIR" reset --hard "origin/${BRANCH}"
  local commit
  commit=$(git -C "$APP_DIR" rev-parse HEAD)
  printf 'Deployed commit %s\n' "$commit"

  (cd "$APP_DIR" && cargo build --release --locked)
  [[ -x "${APP_DIR}/target/release/lanes" ]] || die "release binary was not produced."

  sudo mkdir -p "$DATA_DIR"
  sudo chown "$(id -un):$(id -gn)" "$DATA_DIR"
  local data_file="${DATA_DIR}/chat.phext"
  if [[ -s "$data_file" ]]; then
    printf 'Keeping existing chat data at %s\n' "$data_file"
  else
    printf 'No chat file at %s yet. The first start seeds the demo, and later deploys leave it in place.\n' "$data_file"
  fi

  local dump
  dump=$(ss_listen_dump "$PORT")
  if port_is_foreign "$dump" "$PORT"; then
    printf 'Port %s is taken by something other than Lanes:\n%s\n' "$PORT" "$dump" >&2
    exit 1
  fi

  local unit_tmp
  unit_tmp=$(mktemp)
  cat >"$unit_tmp" <<EOF
[Unit]
Description=Phext Lanes
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$(id -un)
Group=$(id -gn)
WorkingDirectory=${APP_DIR}
Environment=LANES_HOST=127.0.0.1
Environment=LANES_PORT=${PORT}
Environment=LANES_FILE=${data_file}
ExecStart=${APP_DIR}/target/release/lanes --host 127.0.0.1 --port ${PORT} --file ${data_file}
Restart=on-failure
RestartSec=2
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF
  if [[ -f "$UNIT_PATH" ]] && ! sudo cmp -s "$unit_tmp" "$UNIT_PATH"; then
    sudo cp -a "$UNIT_PATH" "${UNIT_PATH}.bak-$(date +%Y%m%d%H%M%S)"
  fi
  sudo cp "$unit_tmp" "$UNIT_PATH"
  rm -f "$unit_tmp"
  sudo systemctl daemon-reload
  sudo systemctl enable "$SERVICE"
  sudo systemctl restart "$SERVICE"

  local health=0
  local attempt
  for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30; do
    if curl -fsS --max-time 2 "http://127.0.0.1:${PORT}/api/health" >/dev/null; then
      health=1
      break
    fi
    sleep 1
    printf 'waiting for 127.0.0.1:%s (%s)\n' "$PORT" "$attempt"
  done
  if [[ "$health" != "1" ]]; then
    sudo systemctl status "$SERVICE" --no-pager || true
    sudo journalctl -u "$SERVICE" -n 40 --no-pager || true
    die "Lanes did not answer on http://127.0.0.1:${PORT}/api/health"
  fi
  printf 'Local health check ok: http://127.0.0.1:%s/api/health\n' "$PORT"

  local mods
  mods=$(sudo apachectl -M 2>/dev/null || sudo /usr/sbin/apachectl -M 2>/dev/null || true)
  local modules_path="/etc/httpd/conf.modules.d/phext-lanes-proxy.conf"
  local modules_backup="" modules_created=0
  if ! printf '%s\n' "$mods" | grep -q 'proxy_module' \
    || ! printf '%s\n' "$mods" | grep -q 'proxy_http_module'; then
    if [[ ! -e /etc/httpd/modules/mod_proxy.so && ! -e /usr/lib64/httpd/modules/mod_proxy.so ]]; then
      die "mod_proxy is not installed. Install httpd, then rerun deploy.sh."
    fi
    local mod_tmp
    mod_tmp=$(mktemp)
    cat >"$mod_tmp" <<'EOF'
LoadModule proxy_module modules/mod_proxy.so
LoadModule proxy_http_module modules/mod_proxy_http.so
EOF
    if [[ -f "$modules_path" ]]; then
      modules_backup="${modules_path}.bak-$(date +%Y%m%d%H%M%S)"
      sudo cp -a "$modules_path" "$modules_backup"
    else
      modules_created=1
    fi
    sudo cp "$mod_tmp" "$modules_path"
    rm -f "$mod_tmp"
  fi

  local vhost="/etc/httpd/conf.d/${DOMAIN}.conf"
  local vhost_backup="" vhost_created=0
  local vhost_tmp
  vhost_tmp=$(mktemp)
  cat >"$vhost_tmp" <<EOF
# Managed by phext-lanes deploy.sh.
# Reverse proxy only. Other vhosts are not edited.
<VirtualHost *:80>
    ServerName ${DOMAIN}
    ProxyPreserveHost On
    ProxyPass / http://127.0.0.1:${PORT}/
    ProxyPassReverse / http://127.0.0.1:${PORT}/
    ErrorLog logs/${DOMAIN}-error.log
    CustomLog logs/${DOMAIN}-access.log combined
</VirtualHost>
EOF
  if vhost_matches "$vhost" "$DOMAIN" "$PORT"; then
    printf 'Apache already proxies %s to 127.0.0.1:%s\n' "$DOMAIN" "$PORT"
    rm -f "$vhost_tmp"
  else
    if [[ -f "$vhost" ]]; then
      vhost_backup="${vhost}.bak-$(date +%Y%m%d%H%M%S)"
      sudo cp -a "$vhost" "$vhost_backup"
      printf 'Backed up %s to %s\n' "$vhost" "$vhost_backup"
    else
      vhost_created=1
    fi
    sudo cp "$vhost_tmp" "$vhost"
    rm -f "$vhost_tmp"
  fi

  local restore_vhost=0 restore_modules=0
  if [[ -n "$vhost_backup" || "$vhost_created" == "1" ]]; then
    restore_vhost=1
  fi
  if [[ -n "$modules_backup" || "$modules_created" == "1" ]]; then
    restore_modules=1
  fi
  if ! sudo apachectl configtest; then
    printf 'apachectl configtest failed. Restoring files this run changed.\n' >&2
    restore_file "$vhost" "$vhost_backup" "$restore_vhost"
    restore_file "$modules_path" "$modules_backup" "$restore_modules"
    exit 1
  fi
  if ! sudo apachectl graceful; then
    printf 'apachectl graceful failed. Restoring files this run changed.\n' >&2
    restore_file "$vhost" "$vhost_backup" "$restore_vhost"
    restore_file "$modules_path" "$modules_backup" "$restore_modules"
    sudo apachectl graceful || true
    exit 1
  fi

  local tls_note="skipped (--no-tls)"
  local tls_failed=0
  local certbot_later
  certbot_later=$(certbot_print)
  if [[ "$NO_TLS" == "1" ]]; then
    printf 'Skipping certbot (--no-tls).\n'
  else
    local public resolved_here=0
    public=$(public_ipv4 || true)
    if dns_points_at "$public" "$DOMAIN"; then
      resolved_here=1
    fi
    local certbot_bin=""
    if command -v certbot >/dev/null 2>&1; then
      certbot_bin=$(command -v certbot)
    elif [[ -x /usr/bin/certbot ]]; then
      certbot_bin=/usr/bin/certbot
    fi
    if [[ "$resolved_here" == "1" && -n "$certbot_bin" ]]; then
      local -a cert_args
      cert_args=(--apache -d "$DOMAIN" --non-interactive --agree-tos --redirect)
      if [[ -n "${CERTBOT_EMAIL:-}" ]]; then
        cert_args+=(--email "$CERTBOT_EMAIL")
      else
        cert_args+=(--register-unsafely-without-email)
      fi
      if sudo "$certbot_bin" "${cert_args[@]}"; then
        tls_note="certbot installed a certificate for ${DOMAIN}"
      else
        tls_note="certbot failed"
        tls_failed=1
        printf 'certbot failed. The HTTP proxy is still in place. Run:\n  %s\n' "$certbot_later" >&2
      fi
    else
      tls_note="not run"
      printf 'TLS was not requested from certbot.\n' >&2
      if [[ "$resolved_here" != "1" ]]; then
        printf '%s does not resolve to this server public IP (%s).\n' "$DOMAIN" "${public:-unknown}" >&2
        printf 'The A record for %s should be %s.\n' "$DOMAIN" "$EXPECTED_A" >&2
      fi
      if [[ -n "$public" && "$public" != "$EXPECTED_A" ]]; then
        printf 'This host public IP is %s. The A record for %s belongs on %s.\n' \
          "$public" "$DOMAIN" "$EXPECTED_A" >&2
      fi
      if [[ -z "$certbot_bin" ]]; then
        printf 'certbot is not installed. Confirm with: certbot certificates\n' >&2
      fi
      printf 'When DNS points here, run:\n  %s\n' "$certbot_later" >&2
    fi
  fi

  local local_code domain_http domain_https phext_code
  local_code=$(http_code "http://127.0.0.1:${PORT}/api/health")
  domain_http=$(http_code "http://${DOMAIN}/")
  domain_https=$(http_code "https://${DOMAIN}/")
  phext_code=$(http_code "https://phext.io/")
  if [[ "$phext_code" != "200" ]]; then
    phext_code=$(http_code "http://phext.io/")
  fi

  printf '\nLanes deploy\n'
  printf '  commit:     %s\n' "$commit"
  printf '  branch:     %s\n' "$BRANCH"
  printf '  listen:     127.0.0.1:%s\n' "$PORT"
  printf '  data:       %s\n' "$data_file"
  printf '  apache:     %s\n' "$vhost"
  printf '  local:      %s\n' "$local_code"
  printf '  http://%s: %s\n' "$DOMAIN" "$domain_http"
  printf '  https://%s: %s\n' "$DOMAIN" "$domain_https"
  printf '  phext.io:   %s\n' "$phext_code"
  printf '  tls:        %s\n' "$tls_note"
  printf '  dns:        %s -> %s\n' "$DOMAIN" "$EXPECTED_A"

  if [[ "$local_code" != "200" ]]; then
    die "local health check returned ${local_code}, expected 200."
  fi
  if [[ "$phext_code" != "200" ]]; then
    die "phext.io returned ${phext_code}, expected 200. Apache backup: ${vhost_backup:-none}."
  fi
  if [[ "$tls_failed" == "1" ]]; then
    exit 1
  fi
}

if [[ "${DEPLOY_SH_SELFTEST:-}" != "1" ]]; then
  main "$@"
fi
