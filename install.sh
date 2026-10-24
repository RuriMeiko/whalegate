#!/bin/sh
# whalegate installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/RuriMeiko/whalegate/main/install.sh | sh
#
# What it does:
#   1. downloads the prebuilt binary for this OS/CPU into ~/.local/bin/whalegate (checksum-verified)
#   2. runs it in the background and on login (systemd --user on Linux, launchd on macOS)
#   3. points dsh at it, if dsh is installed
#   4. prints the dashboard URL — open it and add a DeepSeek account
#
# Options (environment variables):
#   WHALEGATE_VERSION=v0.4.0   pin a version (default: latest release)
#   WHALEGATE_BIN_DIR=/path    install somewhere else (default: ~/.local/bin)
#   WHALEGATE_PORT=8790        dashboard / API port
#   WHALEGATE_NO_SERVICE=1     only install the binary, don't start it
#   WHALEGATE_NO_DSH=1         don't touch the dsh config
set -eu

REPO="RuriMeiko/whalegate"
VERSION="${WHALEGATE_VERSION:-latest}"
BIN_DIR="${WHALEGATE_BIN_DIR:-$HOME/.local/bin}"
PORT="${WHALEGATE_PORT:-8790}"

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64 | Linux-amd64) TARGET=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) TARGET=aarch64-unknown-linux-musl ;;
  Darwin-arm64) TARGET=aarch64-apple-darwin ;;
  Darwin-x86_64) TARGET=x86_64-apple-darwin ;;
  *) die "no prebuilt binary for $(uname -s) $(uname -m) — build from source: https://github.com/$REPO#quick-start" ;;
esac

if [ -n "${WHALEGATE_DOWNLOAD_BASE:-}" ]; then
  BASE="$WHALEGATE_DOWNLOAD_BASE"   # mirrors / testing
elif [ "$VERSION" = latest ]; then
  BASE="https://github.com/$REPO/releases/latest/download"
else
  BASE="https://github.com/$REPO/releases/download/$VERSION"
fi

fetch() { # url dest
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1"
  else die "need curl or wget"; fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM
ARCHIVE="whalegate-$TARGET.tar.gz"

say "Downloading $ARCHIVE ($VERSION)"
fetch "$BASE/$ARCHIVE" "$TMP/$ARCHIVE" || die "download failed: $BASE/$ARCHIVE"

if fetch "$BASE/SHA256SUMS.txt" "$TMP/SHA256SUMS.txt" 2>/dev/null; then
  want="$(grep " $ARCHIVE\$" "$TMP/SHA256SUMS.txt" | cut -d' ' -f1)"
  if command -v sha256sum >/dev/null 2>&1; then got="$(sha256sum "$TMP/$ARCHIVE" | cut -d' ' -f1)"
  else got="$(shasum -a 256 "$TMP/$ARCHIVE" | cut -d' ' -f1)"; fi
  [ -n "$want" ] && [ "$want" = "$got" ] || die "checksum mismatch for $ARCHIVE"
  say "Checksum OK"
fi

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
mkdir -p "$BIN_DIR"
# replace atomically so a running copy keeps working until it restarts
cp "$TMP/whalegate-$TARGET/whalegate" "$BIN_DIR/.whalegate.new"
chmod 755 "$BIN_DIR/.whalegate.new"
mv -f "$BIN_DIR/.whalegate.new" "$BIN_DIR/whalegate"
BIN="$BIN_DIR/whalegate"
say "Installed $("$BIN" version) → $BIN"

wait_up() {
  i=0
  while [ $i -lt 30 ]; do
    if command -v curl >/dev/null 2>&1; then curl -fs "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && return 0
    else wget -qO- "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1 && return 0; fi
    i=$((i + 1)); sleep 1
  done
  return 1
}

if [ "${WHALEGATE_NO_SERVICE:-0}" != 1 ]; then
  case "$(uname -s)" in
    Linux)
      if command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
        UNIT_DIR="$HOME/.config/systemd/user"
        mkdir -p "$UNIT_DIR"
        cat > "$UNIT_DIR/whalegate.service" <<EOF
[Unit]
Description=whalegate (OpenAI-compatible gateway into DeepSeek web)
After=network-online.target

[Service]
ExecStart=$BIN serve
Environment=DS_PORT=$PORT
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
EOF
        systemctl --user daemon-reload
        systemctl --user enable whalegate >/dev/null 2>&1
        systemctl --user restart whalegate
        say "Running as systemd user service (logs: journalctl --user -u whalegate -f)"
        say "Tip: 'loginctl enable-linger $USER' keeps it running while you're logged out"
      else
        nohup env DS_PORT="$PORT" "$BIN" serve >"$HOME/.whalegate.log" 2>&1 &
        say "Started in the background (no systemd --user here; log: ~/.whalegate.log)"
      fi
      ;;
    Darwin)
      PLIST="$HOME/Library/LaunchAgents/io.github.rurimeiko.whalegate.plist"
      mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
      cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>io.github.rurimeiko.whalegate</string>
  <key>ProgramArguments</key><array><string>$BIN</string><string>serve</string></array>
  <key>EnvironmentVariables</key><dict><key>DS_PORT</key><string>$PORT</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$HOME/Library/Logs/whalegate.log</string>
  <key>StandardErrorPath</key><string>$HOME/Library/Logs/whalegate.log</string>
</dict>
</plist>
EOF
      launchctl bootout "gui/$(id -u)/io.github.rurimeiko.whalegate" >/dev/null 2>&1 || true
      launchctl bootstrap "gui/$(id -u)" "$PLIST"
      say "Running as a launchd agent (log: ~/Library/Logs/whalegate.log)"
      ;;
  esac
  wait_up && say "Gateway is up on http://127.0.0.1:$PORT" || say "Gateway did not answer yet — check the log above"
fi

if [ "${WHALEGATE_NO_DSH:-0}" != 1 ]; then
  DSH_PROFILE="${DSH_HOME:-$HOME/.dsh}/profiles/web"
  if [ -d "$DSH_PROFILE" ]; then
    say "Configuring dsh"
    DS_PORT="$PORT" "$BIN" setup-dsh || say "dsh setup failed — run '$BIN setup-dsh' later, or use the dsh button on the dashboard"
  fi
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "Add $BIN_DIR to your PATH to run 'whalegate' directly" ;;
esac

cat <<EOF

  whalegate is installed.

  1. Open the dashboard:   http://127.0.0.1:$PORT/
  2. Add a DeepSeek account (Chrome login or paste a token)
  3. Point any OpenAI client at  http://127.0.0.1:$PORT/v1  with model "deepseek-web"

EOF
