#!/usr/bin/env bash
# Deploy Egress Worker (cổng ra tới DeepSeek) lên Cloudflare.
# Cần: CLOUDFLARE_API_TOKEN (quyền "Edit Cloudflare Workers"), hoặc đã chạy `npx wrangler login`.
# Tuỳ chọn: WG_EGRESS_KEY (bỏ trống thì sinh ngẫu nhiên, lưu ~/.whalegate/egress-key, chỉ chủ máy đọc được)
#           WG_EGRESS_NAME (mặc định ds-egress), CLOUDFLARE_ACCOUNT_ID
set -euo pipefail
WRANGLER="npx --yes wrangler@4.149.0"
cd "$(dirname "$0")"
NAME="${WG_EGRESS_NAME:-ds-egress}"
KEY_FILE="$HOME/.whalegate/egress-key"
if [ -z "${WG_EGRESS_KEY:-}" ]; then
  if [ -s "$KEY_FILE" ]; then WG_EGRESS_KEY=$(cat "$KEY_FILE")
  else
    WG_EGRESS_KEY=$(openssl rand -hex 24)
    mkdir -p "$(dirname "$KEY_FILE")"; (umask 077; printf '%s' "$WG_EGRESS_KEY" > "$KEY_FILE")
    echo "đã sinh khoá mới → $KEY_FILE"
  fi
fi
$WRANGLER whoami >/dev/null || { echo "lỗi: chưa đăng nhập Cloudflare (CLOUDFLARE_API_TOKEN hoặc npx wrangler login)" >&2; exit 1; }
LOG=$(mktemp); trap 'rm -f "$LOG"' EXIT
$WRANGLER deploy --config wrangler-egress.toml --name "$NAME" 2>&1 | tee "$LOG"
URL=$(grep -oE 'https://[a-zA-Z0-9.-]+\.workers\.dev' "$LOG" | head -1 || true)
printf '%s' "$WG_EGRESS_KEY" | $WRANGLER secret put EGRESS_KEY --config wrangler-egress.toml --name "$NAME" >/dev/null && echo "EGRESS_KEY ✓"
[ -n "$URL" ] || { echo "deploy xong nhưng không đọc được địa chỉ workers.dev — xem log"; exit 0; }
for i in $(seq 10); do [ "$(curl -s -o /dev/null -w '%{http_code}' "$URL/__health")" = 200 ] && break; sleep 2; done
echo "== kiểm tra"
echo "  /__health          → $(curl -s -o /dev/null -w '%{http_code}' "$URL/__health") (mong đợi 200)"
echo "  không khoá         → $(curl -s -o /dev/null -w '%{http_code}' "$URL/api/v0/users/current") (mong đợi 401)"
echo "  IP đầu ra          → $(curl -s -H "x-wg-key: $WG_EGRESS_KEY" "$URL/__ip")"
cat <<MSG

Xong. Bật trong whalegate (Cài đặt → Proxy, cần khởi động lại):
  DS_EGRESS_URL = $URL
  DS_EGRESS_KEY = (nội dung $KEY_FILE)
MSG
