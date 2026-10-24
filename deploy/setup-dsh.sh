#!/usr/bin/env bash
# Cấu hình dsh dùng ds-harness: 1 model `deepseek-web`, reasoning off(none)/low(auto)/max, ảnh, ctx 500K,
# agent preset mặc định PTC (model viết 1 chương trình TS gọi nhiều tool → ít lượt request lên DS web hơn).
# Dùng:  deploy/setup-dsh.sh [profile_dir] [base_url]
#   profile_dir mặc định ~/.dsh/profiles/web      base_url mặc định http://127.0.0.1:8790/v1
#   DSH_PRESET=standard deploy/setup-dsh.sh  → giữ preset thường (mặc định: ptc)
# Chạy lại bao nhiêu lần cũng được (idempotent); luôn tạo file .bak.<ngày-giờ> trước khi sửa.
set -euo pipefail
PROFILE="${1:-$HOME/.dsh/profiles/web}"
BASE="${2:-http://127.0.0.1:8790/v1}"
CTX="${DS_CONTEXT_TOKENS:-500000}"
PRESET="${DSH_PRESET:-ptc}"
FILE="$PROFILE/cordis.patch.yml"
[ -f "$FILE" ] || { echo "không thấy $FILE — truyền đúng thư mục profile của dsh" >&2; exit 1; }
cp "$FILE" "$FILE.bak.$(date +%Y%m%d-%H%M%S)"
PROFILE_FILE="$FILE" BASE="$BASE" CTX="$CTX" PRESET="$PRESET" python3 - <<'PY'
import os, sys
try:
    import yaml
except ImportError:
    sys.exit("cần PyYAML: pip install pyyaml")
p, base, ctx = os.environ["PROFILE_FILE"], os.environ["BASE"], int(os.environ["CTX"])
d = yaml.safe_load(open(p)) or []
provider = {
    "displayName": "DeepSeek Harness (free pool)",
    "apiKeyEnv": os.environ.get("DS_KEY_ENV", "DS_HARNESS_API_KEY"),
    "api": "openai-completions",
    "baseURL": base,
    "timeoutMs": 600000,
    "compat": {"supportsDeveloperRole": False, "supportsReasoningEffort": True, "supportsUsageInStreaming": True},
    "models": [{
        "id": "deepseek-web", "name": "DeepSeek Web (harness, free)",
        "contextWindow": ctx, "maxTokens": 32768, "input": ["text", "image"],
        "reasoningEfforts": {"off": "none", "low": "auto", "max": "max"},
    }],
}
def entry(i, name):
    for e in d:
        if e.get("id") == i: return e
    e = {"id": i, "name": name, "config": {}}; d.append(e); return e
e = entry("llm-pi-ai", "@deepseek-ai/dsh-llm-pi-ai")
e.setdefault("config", {}).setdefault("providers", {})["ds-harness"] = provider
m = entry("agent-default-model", "@deepseek-ai/dsh-agent-default-model")
m.setdefault("config", {}).update({"provider": "ds-harness", "model": "deepseek-web"})
# preset chọn sẵn cho phiên mới; `default` (preset dự phòng) giữ nguyên nếu đã có
r = entry("agent-preset-registry", "@deepseek-ai/dsh-agent-preset-registry").setdefault("config", {})
r.setdefault("default", "standard")
r["selectedDefault"] = os.environ["PRESET"]
yaml.safe_dump(d, open(p, "w"), allow_unicode=True, sort_keys=False)
print("OK:", p, "| preset:", os.environ["PRESET"])
PY
echo "Xong. Mở lại dsh rồi chọn 'DeepSeek Web (harness, free)'. Nếu harness có DS_API_KEY thì đặt biến ${DS_KEY_ENV:-DS_HARNESS_API_KEY} trong ~/.dsh/.env"
