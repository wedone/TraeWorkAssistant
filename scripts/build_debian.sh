#!/usr/bin/env bash
# 构建 Debian 原生发布包：aiwork-server 单二进制 + 前端 dist + systemd 单元 + 安装脚本
#
# 用法：bash scripts/build_debian.sh
# 前置：Debian 12（glibc 2.36）+ Node.js 20 + Rust 1.89+
# 产物：release/aiwork-server_<版本>_linux_<架构>.tar.gz 与 .sha256
#
# 注意：必须在 Debian 12 或 glibc 不高于 2.36 的环境构建；
#       在更高版本发行版构建的二进制可能无法在 Debian 12 运行。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v node >/dev/null 2>&1 || { echo "缺少 node（构建期需要 Node.js 20）" >&2; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "缺少 cargo（构建期需要 Rust 1.89+）" >&2; exit 1; }

VERSION="$(node -p "require('./package.json').version")"
ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
  x86_64|amd64) ARCH="amd64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  *) echo "不支持的架构：$ARCH_RAW" >&2; exit 1 ;;
esac

echo "==> 构建前端（v${VERSION}）"
npm ci --no-audit --no-fund
npm run build

echo "==> 构建服务端（release）"
cargo build --release -p aiwork-server

OUT_DIR="$ROOT/release"
STAGE_NAME="aiwork-server_${VERSION}_linux_${ARCH}"
STAGE="$OUT_DIR/$STAGE_NAME"

echo "==> 组装发布包：$STAGE_NAME"
rm -rf "$STAGE"
install -d "$STAGE/dist" "$STAGE/deploy/systemd"

install -m 0755 target/release/aiwork-server "$STAGE/aiwork-server"
cp -R dist/. "$STAGE/dist/"
install -m 0644 deploy/systemd/aiwork-server.service "$STAGE/deploy/systemd/"
install -m 0600 deploy/aiwork.env.example "$STAGE/deploy/"
install -m 0755 deploy/install.sh "$STAGE/deploy/"
install -m 0644 docs/server-deploy.md "$STAGE/server-deploy.md"

tar -C "$OUT_DIR" -czf "$OUT_DIR/$STAGE_NAME.tar.gz" "$STAGE_NAME"
(
  cd "$OUT_DIR"
  sha256sum "$STAGE_NAME.tar.gz" > "$STAGE_NAME.tar.gz.sha256"
)

echo
echo "产物："
echo "  $OUT_DIR/$STAGE_NAME.tar.gz"
echo "  $OUT_DIR/$STAGE_NAME.tar.gz.sha256"
