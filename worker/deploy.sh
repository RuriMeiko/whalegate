#!/usr/bin/env bash
# Deploy Worker whalegate lên Cloudflare, không cần bấm gì trên dashboard.
#
# Cần (biến môi trường):
#   CLOUDFLARE_API_TOKEN  token Cloudflare của bạn, quyền "Edit Cloudflare Workers" (xem README)
#   WG_UPSTREAMS          địa chỉ công khai các instance whalegate, cách nhau bằng dấu phẩy
#                         ví dụ: https://wg1.example.com,https://wg2.example.com
# Tuỳ chọn:
#   CLOUDFLARE_ACCOUNT_ID bắt buộc nếu token thấy nhiều account
#   WG_UPSTREAM_TOKEN     DS_API_KEY đã đặt trên các instance (khuyên dùng — instance mở ra ngoài mà
#                         không có DS_API_KEY sẽ tự từ chối request từ Worker)
#   WG_WORKER_TOKEN       bearer client gửi tới Worker; bỏ trống thì sinh ngẫu nhiên và lưu vào
#                         ~/.whalegate/worker-token (chỉ chủ máy đọc được)
#   WG_WORKER_NAME        tên Worker (mặc định ds-proxy)
set -euo pipefail

WRANGLER="npx --yes wrangler@4.149.0"
cd "$(dirname "$0")"

die() { echo "lỗi: $*" >&2; exit 1; }
[ -n "${CLOUDFLARE_API_TOKEN:-}" ] || die "thiếu CLOUDFLARE_API_TOKEN"
[ -n "${WG_UPSTREAMS:-}" ] || die "thiếu WG_UPSTREAMS (vd https://wg1.example.com,https://wg2.example.com)"
NAME="${WG_WORKER_NAME:-ds-proxy}"

# UPSTREAMS → JSON array, kiểm tra từng địa chỉ là https công khai
UPSTREAMS_JSON=$(python3 - "$WG_UPSTREAMS" <<'PY'
import json, sys, urllib.parse
out = []
for u in sys.argv[1].split(","):
    u = u.strip().rstrip("/")
    if not u:
        continue
    p = urllib.parse.urlparse(u)
    if p.scheme != "https" or not p.hostname:
        sys.exit(f"upstream phải là https://…: {u}")
    if p.hostname in ("localhost", "127.0.0.1", "::1") or p.hostname.endswith(".local"):
        sys.exit(f"Cloudflare không gọi được địa chỉ nội bộ: {u} — mở instance ra ngoài (vd Cloudflare Tunnel) rồi dùng địa chỉ đó")
    out.append(u)
if not out:
    sys.exit("WG_UPSTREAMS rỗng")
print(json.dumps(out))
PY
) || die "WG_UPSTREAMS không hợp lệ"

TOKEN_FILE="$HOME/.whalegate/worker-token"
if [ -z "${WG_WORKER_TOKEN:-}" ]; then
  if [ -s "$TOKEN_FILE" ]; then
    WG_WORKER_TOKEN=$(cat "$TOKEN_FILE")
  else
    WG_WORKER_TOKEN=$(openssl rand -hex 24)
    mkdir -p "$(dirname "$TOKEN_FILE")"
    (umask 077; printf '%s' "$WG_WORKER_TOKEN" > "$TOKEN_FILE")
    echo "đã sinh WORKER_TOKEN mới → $TOKEN_FILE"
  fi
fi

echo "== kiểm tra token Cloudflare"
$WRANGLER whoami >/dev/null || die "token Cloudflare không dùng được (sai, hết hạn, hoặc thiếu quyền)"

echo "== deploy Worker $NAME"
DEPLOY_LOG=$(mktemp)
trap 'rm -f "$DEPLOY_LOG"' EXIT
$WRANGLER deploy --name "$NAME" 2>&1 | tee "$DEPLOY_LOG"
URL=$(grep -oE 'https://[a-zA-Z0-9.-]+\.workers\.dev' "$DEPLOY_LOG" | head -1 || true)

echo "== đặt secret"
put() { printf '%s' "$2" | $WRANGLER secret put "$1" --name "$NAME" >/dev/null && echo "  $1 ✓"; }
put UPSTREAMS "$UPSTREAMS_JSON"
put WORKER_TOKEN "$WG_WORKER_TOKEN"
if [ -n "${WG_UPSTREAM_TOKEN:-}" ]; then put UPSTREAM_TOKEN "$WG_UPSTREAM_TOKEN"; fi

[ -n "$URL" ] || { echo "deploy xong nhưng không đọc được địa chỉ workers.dev — xem log ở trên"; exit 0; }

echo "== kiểm tra $URL"
# secret mới cần vài giây để có hiệu lực
for i in $(seq 10); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' "$URL/health")" = 200 ] && break
  sleep 2
done
check() { printf '  %-40s → %s (mong đợi %s)\n' "$1" "$2" "$3"; [ "$2" = "$3" ] || FAIL=1; }
FAIL=0
check "/health" "$(curl -s -o /dev/null -w '%{http_code}' "$URL/health")" 200
check "/v1/models không token" "$(curl -s -o /dev/null -w '%{http_code}' "$URL/v1/models")" 401
check "/api/accounts (admin, phải bị chặn)" "$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $WG_WORKER_TOKEN" "$URL/api/accounts")" 404
code=$(curl -s -o /dev/null -w '%{http_code}' -H "authorization: Bearer $WG_WORKER_TOKEN" "$URL/v1/models")
check "/v1/models có token (tới upstream)" "$code" 200
if [ "$code" = 403 ]; then echo "  → upstream từ chối: đặt DS_API_KEY trên instance và truyền WG_UPSTREAM_TOKEN giống vậy"; fi
if [ "$code" = 401 ]; then echo "  → WG_UPSTREAM_TOKEN không khớp DS_API_KEY của instance"; fi

cat <<EOF

Xong. Client trỏ vào:
  base_url = $URL/v1
  api_key  = (nội dung $TOKEN_FILE, hoặc WG_WORKER_TOKEN bạn đã truyền)
EOF
exit $FAIL
