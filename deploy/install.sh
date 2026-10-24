#!/usr/bin/env bash
# AI Work 助手 Web 版 Debian 原生安装脚本（systemd）
#
# 用法：sudo bash deploy/install.sh
# 前置：Debian 12 / systemd；在已解压的发布包根目录执行
# 行为：创建 aiwork 系统用户 → 安装到 /opt/aiwork → 生成 /etc/aiwork/aiwork.env
#       → 安装并启动 aiwork-server.service（幂等，可重复执行用于升级）

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="/opt/aiwork"
DATA_DIR="/var/lib/aiwork"
ENV_DIR="/etc/aiwork"
ENV_FILE="$ENV_DIR/aiwork.env"
SERVICE_NAME="aiwork-server"
SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"

if [[ ${EUID} -ne 0 ]]; then
  echo "请以 root 运行：sudo bash deploy/install.sh" >&2
  exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
  echo "未检测到 systemd，本脚本仅适用于 systemd 发行版" >&2
  exit 1
fi

for f in \
  "$ROOT/aiwork-server" \
  "$ROOT/dist/index.html" \
  "$ROOT/deploy/aiwork.env.example" \
  "$ROOT/deploy/systemd/${SERVICE_NAME}.service"; do
  if [[ ! -e "$f" ]]; then
    echo "发布包不完整，缺少：$f" >&2
    exit 1
  fi
done

# 1) 数据目录（先建目录，再建用户，避免首次安装时归属错误）
install -d -m 0700 "$DATA_DIR"

# 2) 系统用户（无登录 shell）
if ! id aiwork >/dev/null 2>&1; then
  useradd --system --home-dir "$DATA_DIR" --shell /usr/sbin/nologin aiwork
  echo "已创建系统用户 aiwork"
fi
chown aiwork:aiwork "$DATA_DIR"

# 3) 程序与前端静态资源
# 升级场景：先停服务，避免替换 dist 期间页面资源短暂缺失
systemctl stop "$SERVICE_NAME" >/dev/null 2>&1 || true
install -d -m 0755 "$PREFIX"
install -m 0755 "$ROOT/aiwork-server" "$PREFIX/aiwork-server"
rm -rf "$PREFIX/dist"
install -d -m 0755 "$PREFIX/dist"
cp -R "$ROOT/dist/." "$PREFIX/dist/"

# 4) 环境配置（已存在则保留，避免覆盖用户修改）
install -d -m 0755 "$ENV_DIR"
if [[ ! -f "$ENV_FILE" ]]; then
  install -m 0600 "$ROOT/deploy/aiwork.env.example" "$ENV_FILE"
  echo "已生成 $ENV_FILE（可编辑后重启服务）"
else
  echo "保留已有 $ENV_FILE"
fi

# 5) systemd 单元
install -m 0644 "$ROOT/deploy/systemd/${SERVICE_NAME}.service" "$SERVICE_FILE"
systemctl daemon-reload
systemctl enable "$SERVICE_NAME" >/dev/null
systemctl start "$SERVICE_NAME"

# 启动结果检查（配置错误 / 端口占用 / glibc 不匹配时尽早暴露）
for _ in 1 2 3 4 5; do
  systemctl is-active --quiet "$SERVICE_NAME" && break
  sleep 1
done
if ! systemctl is-active --quiet "$SERVICE_NAME"; then
  echo "服务启动失败，最近日志：" >&2
  journalctl -u "$SERVICE_NAME" --no-pager -n 50 >&2 || true
  exit 1
fi

echo
echo "安装完成。"
echo "  服务状态：systemctl status $SERVICE_NAME"
echo "  实时日志：journalctl -u $SERVICE_NAME -f"
echo "  管理令牌：cat $DATA_DIR/conf/admin_token"
echo "  数据目录：$DATA_DIR"
echo
echo "若需调整配置，请编辑 $ENV_FILE 后执行 systemctl restart $SERVICE_NAME"
