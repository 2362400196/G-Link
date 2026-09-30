#!/usr/bin/env bash
# =============================================================
# G-Link relay 服务端一键安装/升级脚本
#
# 全新安装（服务器上执行，令牌与客户端节点配置保持一致）：
#   curl -fsSL https://gitee.com/zhuxiaohuaqn/g-link/raw/main/install.sh -o install.sh
#   bash install.sh --token 你的令牌
#
# 升级（重复运行即可更新二进制；不传 --token 则沿用已配置的令牌）：
#   bash install.sh
# =============================================================
set -euo pipefail

REPO_RAW="https://gitee.com/zhuxiaohuaqn/g-link/raw/main"
BIN_DIR="/opt/pubg-relay"
BIN_PATH="$BIN_DIR/pubg-relay-linux"
SERVICE="pubg-relay"
PORT=41000

log() { printf '\033[32m[install]\033[0m %s\n' "$*"; }
err() { printf '\033[31m[错误]\033[0m %s\n' "$*" >&2; exit 1; }

# ---- 参数解析 ----
TOKEN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --token) TOKEN="${2:-}"; shift 2 ;;
    --port)  PORT="${2:-41000}"; shift 2 ;;
    -h|--help) echo "用法: install.sh [--token 令牌] [--port 41000]"; exit 0 ;;
    *) err "未知参数: $1" ;;
  esac
done

[ "$(id -u)" -eq 0 ] || err "请用 root 运行：sudo bash install.sh ..."

# ---- 令牌：未传参且已安装过则沿用原令牌（升级场景） ----
if [ -z "$TOKEN" ] && [ -f "/etc/systemd/system/$SERVICE.service" ]; then
  TOKEN=$(grep -oP '(?<=--token )\S+' "/etc/systemd/system/$SERVICE.service" 2>/dev/null || true)
  [ -n "$TOKEN" ] && log "检测到已安装服务，沿用原令牌"
fi
[ -n "$TOKEN" ] || err "缺少令牌：bash install.sh --token 你的令牌"

# ---- 环境检查 ----
[ "$(uname -m)" = "x86_64" ] || err "仅支持 x86_64，当前架构: $(uname -m)"
command -v curl >/dev/null 2>&1 || command -v wget >/dev/null 2>&1 \
  || err "缺少 curl/wget，请先安装：apt install -y curl"

# ---- 下载二进制 ----
mkdir -p "$BIN_DIR"
TMP="$BIN_DIR/pubg-relay-linux.tmp"
log "从仓库下载服务端二进制..."
if command -v curl >/dev/null 2>&1; then
  curl -fL --retry 3 -o "$TMP" "$REPO_RAW/bin/pubg-relay-linux"
else
  wget -qO "$TMP" --tries=3 "$REPO_RAW/bin/pubg-relay-linux"
fi

# 校验 ELF 魔数，防止把网络错误页当程序装进去
[ "$(head -c 4 "$TMP" | od -An -tx1 | tr -d ' \n')" = "7f454c46" ] \
  || err "下载内容不是有效二进制（检查网络后重试）"

chmod 755 "$TMP"
# 先停服务再原子替换，避免 Text file busy
systemctl stop "$SERVICE" 2>/dev/null || true
mv -f "$TMP" "$BIN_PATH"
log "二进制已就位: $BIN_PATH"

# ---- 生成 systemd 服务 ----
cat > "/etc/systemd/system/$SERVICE.service" <<EOF
[Unit]
Description=G-Link relay server (UDP tunnel v2)
After=network.target

[Service]
ExecStart=$BIN_PATH --bind 0.0.0.0:$PORT --token $TOKEN
Restart=always
RestartSec=3
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "$SERVICE" >/dev/null 2>&1 || true
systemctl restart "$SERVICE"
sleep 1

# ---- 验证 ----
if systemctl is-active --quiet "$SERVICE"; then
  log "================ 安装完成 ================"
  log "服务状态 : active (running)"
  log "监听端口 : UDP $PORT（防火墙需放行）"
  log "实时日志 : journalctl -u $SERVICE -f"
  log "客户端接入: 节点地址 <本机IP>:$PORT  令牌 $TOKEN"
else
  err "服务启动失败，排查: journalctl -u $SERVICE --no-pager -n 30"
fi
