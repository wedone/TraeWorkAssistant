#!/usr/bin/env bash
# =============================================================================
# AI Work 助手 Web 版（aiwork-server）—— Debian 12 裸机一键部署 / 升级脚本
#
# 用法（需 root；脚本自包含，不依赖仓库其他文件）：
#   # 方式 A：直接下载执行，无需克隆仓库
#   curl -fsSL https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/install.sh | sudo bash
#
#   # 方式 B：克隆后本地执行
#   sudo bash deploy/install.sh              # 交互式，所有参数都有默认值（回车即用）
#   sudo bash deploy/install.sh -y           # 全默认、不交互（适合脚本化重复执行）
#   sudo bash deploy/install.sh --local ./aiwork-server-linux-amd64.tar.gz
#
# 做的事：下载 CI 产物 → 落盘 → 生成 systemd 单元 → 探活 → 打印访问地址与管理令牌。
# 重复执行 = 升级：数据目录与密钥全部保留，失败自动回滚上一个二进制。
#
# 为什么不需要 Docker：镜像里那层「Docker 特化」只是环境变量（AIWORK_DATA_DIR /
# AIWORK_WEB_DIST / AIWORK_LISTEN_ADDR / AIWORK_DISABLE_LOCAL_QUOTA），本脚本用
# systemd EnvironmentFile 注入等价取值，代码零改动。
# =============================================================================

set -Eeuo pipefail

SCRIPT_VERSION="1.0"
DEFAULT_REPO="wedone/TraeWorkAssistant"
DEFAULT_CHANNEL="baremetal-latest"
DEFAULT_INSTALL_DIR="/opt/aiwork"
DEFAULT_DATA_DIR="/var/lib/aiwork"
DEFAULT_USER="aiwork"
DEFAULT_PORT="8080"
DEFAULT_TZ="Asia/Shanghai"
ENV_FILE="/etc/aiwork/aiwork.env"
UNIT_FILE="/etc/systemd/system/aiwork-server.service"
SERVICE="aiwork-server"

# ── 输出工具 ─────────────────────────────────────────────────────────────────
c_info='\033[1;36m'; c_ok='\033[1;32m'; c_warn='\033[1;33m'; c_err='\033[1;31m'; c_off='\033[0m'
say()  { printf "${c_info}==>${c_off} %s\n" "$*"; }
ok()   { printf "${c_ok}  ✓${c_off} %s\n" "$*"; }
warn() { printf "${c_warn}  !${c_off} %s\n" "$*" >&2; }
die()  { printf "${c_err}  ✗${c_off} %s\n" "$*" >&2; exit 1; }
step() { printf "\n${c_info}[%s]${c_off} %s\n" "$1" "$2"; }

TMP_DIR=""
cleanup() {
  if [[ -n "$TMP_DIR" && -d "$TMP_DIR" ]]; then rm -rf "$TMP_DIR"; fi
  return 0
}
trap cleanup EXIT

# ── 参数解析 ─────────────────────────────────────────────────────────────────
ASSUME_YES=0
FORCE_RESET_VAULT=0
LOCAL_PKG=""
OPT_REPO=""; OPT_CHANNEL=""; OPT_INSTALL_DIR=""; OPT_DATA_DIR=""
OPT_USER=""; OPT_PORT=""; OPT_TZ=""

usage() {
  cat <<'EOF'
用法: install.sh [选项]

直接执行（无需克隆仓库）:
  curl -fsSL https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/install.sh | sudo bash
  curl -fsSL .../deploy/install.sh | sudo bash -s -- -y        # 传参需用 -s --

  -y, --yes              全部使用默认值，不交互
      --local <文件>     使用本地安装包（离线部署）
      --channel <tag>    指定 Release 渠道（默认自动探测）
      --repo <owner/repo> 指定 GitHub 仓库（默认 wedone/TraeWorkAssistant）
      --install-dir <目录> 程序目录（默认 /opt/aiwork）
      --data-dir <目录>    数据目录（默认 /var/lib/aiwork）
      --user <用户名>      运行用户（默认 aiwork）
      --port <端口>        监听端口（默认 8080）
      --tz <时区>          时区（默认 Asia/Shanghai）
      --force-reset-vault 允许在「已有加密凭据但密钥丢失」时继续（会废弃存量凭据）
  -h, --help             显示本帮助
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -y|--yes)          ASSUME_YES=1; shift ;;
    --local)           LOCAL_PKG="${2:-}"; shift 2 ;;
    --channel)         OPT_CHANNEL="${2:-}"; shift 2 ;;
    --repo)            OPT_REPO="${2:-}"; shift 2 ;;
    --install-dir)     OPT_INSTALL_DIR="${2:-}"; shift 2 ;;
    --data-dir)        OPT_DATA_DIR="${2:-}"; shift 2 ;;
    --user)            OPT_USER="${2:-}"; shift 2 ;;
    --port)            OPT_PORT="${2:-}"; shift 2 ;;
    --tz)              OPT_TZ="${2:-}"; shift 2 ;;
    --force-reset-vault) FORCE_RESET_VAULT=1; shift ;;
    -h|--help)         usage; exit 0 ;;
    *)                 die "未知参数: $1（--help 查看用法）" ;;
  esac
done

INTERACTIVE=0
if [[ -t 0 ]]; then INTERACTIVE=1; fi
# curl ... | bash 时 stdin 是脚本本体（非 TTY），但控制终端仍可交互：
# 此时改从 /dev/tty 读，否则交互问答会读走脚本内容、表现为「一路跳过」。
# 用实际打开测试而非 -r 判断：无控制终端时 /dev/tty 存在但 open 会失败。
TTY_IN=""
if : < /dev/tty 2>/dev/null; then TTY_IN="/dev/tty"; fi
if [[ $INTERACTIVE -eq 0 && -n "$TTY_IN" ]]; then INTERACTIVE=1; fi
if [[ $ASSUME_YES -eq 1 ]]; then INTERACTIVE=0; fi

# 统一的读取入口：优先控制终端（管道执行时 stdin 是脚本本体，不能读）；
# 没有控制终端时退回 stdin（仅当 stdin 是 TTY）。都不可用则返回失败。
read_ans() {
  local __var="$1" __val=""
  if [[ -n "$TTY_IN" ]]; then
    IFS= read -r __val < "$TTY_IN" || true
  elif [[ -t 0 ]]; then
    IFS= read -r __val || true
  else
    return 1
  fi
  printf -v "$__var" '%s' "$__val"
  return 0
}

ask() {
  local prompt="$1" default="$2" ans=""
  if [[ $INTERACTIVE -eq 1 ]]; then
    printf '  %s [%s]: ' "$prompt" "$default" >&2
    read_ans ans || true
  fi
  printf '%s' "${ans:-$default}"
}

confirm() {
  local prompt="$1" default="${2:-n}" ans=""
  if [[ $ASSUME_YES -eq 1 ]]; then return 0; fi
  if [[ $INTERACTIVE -eq 0 ]]; then
    # 非交互：按默认值决定（默认 y 即视为同意）
    if [[ "$default" == "y" ]]; then return 0; else return 1; fi
  fi
  printf '  %s [%s]: ' "$prompt" "$default" >&2
  read_ans ans || true
  ans="${ans:-$default}"
  if [[ "$ans" == "y" || "$ans" == "Y" || "$ans" == "yes" ]]; then return 0; else return 1; fi
}

# 破坏性确认：无视 -y，必须人工输入 y。
# 无可用终端时一律拒绝（避免在无人值守/管道场景下静默销毁数据）。
confirm_destructive() {
  local prompt="$1" ans=""
  if [[ -z "$TTY_IN" && ! -t 0 ]]; then
    warn "当前无交互终端，破坏性操作一律拒绝（确认放弃存量凭据请加 --force-reset-vault）"
    return 1
  fi
  if [[ $ASSUME_YES -eq 1 ]]; then
    warn "已指定 -y，但破坏性操作仍需人工确认（确认放弃存量凭据请加 --force-reset-vault）"
  fi
  printf '  %s [n]: ' "$prompt" >&2
  if ! read_ans ans; then
    warn "无法读取确认输入，按拒绝处理"
    return 1
  fi
  if [[ "$ans" == "y" || "$ans" == "Y" || "$ans" == "yes" ]]; then return 0; else return 1; fi
}

# 读已有 env 文件中的某个键（不存在则输出空）
env_get() {
  local key="$1" file="$2" line=""
  if [[ -f "$file" ]]; then
    line="$(grep -m1 "^${key}=" "$file" 2>/dev/null || true)"
  fi
  if [[ -n "$line" ]]; then printf '%s' "${line#*=}"; fi
  return 0
}

gen_hex() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }

# 自动选择可用的 Release 渠道：优先 baremetal-latest（main 分支构建），
# 若尚未有 main 的构建（例如只在 docker_main 上跑过 CI），回落到最新的 baremetal-* 渠道。
# 目的是让「还没合到 main」时直接执行脚本也不会 404。
resolve_channel() {
  local repo="$1" body raw line tag first
  body="$(curl -fsSL --connect-timeout 10 --max-time 20 \
          "https://api.github.com/repos/${repo}/releases?per_page=100" 2>/dev/null || true)"
  if [[ -z "$body" ]]; then printf 'baremetal-latest'; return 0; fi
  # 去掉空白后逐条提取 tag_name（GitHub API 按 created_at 倒序，首个即最新）
  raw="$(printf '%s' "$body" | tr -d ' \n\t' | grep -o '"tag_name":"baremetal-[^"]*"' || true)"
  if [[ -z "$raw" ]]; then printf 'baremetal-latest'; return 0; fi
  first=""
  while IFS= read -r line; do
    tag="${line#*\"tag_name\":\"}"
    tag="${tag%\"}"
    if [[ -z "$first" ]]; then first="$tag"; fi
    if [[ "$tag" == "baremetal-latest" ]]; then
      printf 'baremetal-latest'
      return 0
    fi
  done <<< "$raw"
  printf '%s' "${first:-baremetal-latest}"
}

# ── 0. 前置检查 ──────────────────────────────────────────────────────────────
step 0/8 "环境检查"

[[ $EUID -eq 0 ]] || die "需要 root 权限：请在命令前加 sudo"
[[ "$(uname -s)" == "Linux" ]] || die "本脚本仅支持 Linux"
[[ "$(uname -m)" == "x86_64" ]] || die "当前安装包为 x86_64；本机架构 $(uname -m) 不受支持"
command -v systemctl >/dev/null 2>&1 || die "未找到 systemd（Debian 12 默认自带）"

OS_ID=""; OS_VER=""
if [[ -r /etc/os-release ]]; then
  # shellcheck disable=SC1091
  OS_ID="$(. /etc/os-release; printf '%s' "${ID:-}")"
  OS_VER="$(. /etc/os-release; printf '%s' "${VERSION_ID:-}")"
fi
if [[ "$OS_ID" != "debian" || "$OS_VER" != "12" ]]; then
  warn "检测到 ${OS_ID:-未知} ${OS_VER:-?}，脚本按 Debian 12（glibc 2.36）验证；其他版本可能因 glibc 过旧无法启动"
  confirm "继续部署？" n || die "已取消"
else
  ok "Debian 12 / x86_64"
fi

# 依赖包（只装缺的；不碰 Docker）
NEED_APT=()
command -v curl        >/dev/null 2>&1 || NEED_APT+=("curl")
command -v tar         >/dev/null 2>&1 || NEED_APT+=("tar")
command -v gzip        >/dev/null 2>&1 || NEED_APT+=("gzip")
command -v sha256sum   >/dev/null 2>&1 || NEED_APT+=("coreutils")
command -v od          >/dev/null 2>&1 || NEED_APT+=("coreutils")
[[ -d /usr/share/zoneinfo ]]                     || NEED_APT+=("tzdata")
[[ -f /etc/ssl/certs/ca-certificates.crt ]]      || NEED_APT+=("ca-certificates")

if [[ ${#NEED_APT[@]} -gt 0 ]]; then
  mapfile -t NEED_APT < <(printf '%s\n' "${NEED_APT[@]}" | sort -u)
  say "安装缺失的运行时依赖: ${NEED_APT[*]}"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq || die "apt-get update 失败，请检查网络/软件源"
  apt-get install -y --no-install-recommends "${NEED_APT[@]}" >/dev/null \
    || die "依赖安装失败，请手动执行: apt-get install -y ${NEED_APT[*]}"
  ok "依赖安装完成"
else
  ok "运行时依赖齐备（curl / tar / tzdata / ca-certificates）"
fi

# ── 1. 部署参数（全部带默认值，回车即用）─────────────────────────────────────
step 1/8 "部署参数"

INSTALL_DIR="${OPT_INSTALL_DIR:-$(ask '程序安装目录' "$DEFAULT_INSTALL_DIR")}"
DATA_DIR="${OPT_DATA_DIR:-$(ask '数据目录（SQLite/密钥/日志）' "$DEFAULT_DATA_DIR")}"
RUN_USER="${OPT_USER:-$(ask '运行用户（无登录权限的系统用户）' "$DEFAULT_USER")}"
PORT="${OPT_PORT:-$(ask '监听端口' "$DEFAULT_PORT")}"
TZ_NAME="${OPT_TZ:-$(ask '时区' "$DEFAULT_TZ")}"

[[ "$PORT" =~ ^[0-9]+$ ]] && (( 10#$PORT > 0 && 10#$PORT < 65536 )) || die "端口非法: $PORT"
PORT=$((10#$PORT))   # 归一（去前导零，避免 systemd/curl 侧再解析）
# 去尾部斜杠，避免后续 ${DATA_DIR##*/} 之类的路径运算取到空串
INSTALL_DIR="${INSTALL_DIR%/}"
DATA_DIR="${DATA_DIR%/}"
[[ "$INSTALL_DIR" == /* ]] || die "安装目录必须是绝对路径"
[[ "$DATA_DIR" == /* ]]    || die "数据目录必须是绝对路径"
[[ "$RUN_USER" =~ ^[a-z_][a-z0-9_-]*$ ]] || die "用户名非法: $RUN_USER"
# systemd 单元开了 ProtectHome=true / ProtectSystem=strict，路径落在受限区会被拒绝读写
case "$DATA_DIR" in
  /home/*|/root/*) die "数据目录不能位于 /home 或 /root 下（服务启用 ProtectHome=true）: $DATA_DIR" ;;
esac
case "$INSTALL_DIR" in
  /home/*|/root/*) die "安装目录不能位于 /home 或 /root 下（服务启用 ProtectHome=true）: $INSTALL_DIR" ;;
esac

UPGRADE=0
if [[ -f "$UNIT_FILE" ]]; then UPGRADE=1; fi

# 渠道 / 来源
CHANNEL="${OPT_CHANNEL:-$DEFAULT_CHANNEL}"
REPO="${OPT_REPO:-$DEFAULT_REPO}"
PKG_URL=""
LOCAL_SRC=""

if [[ -n "$LOCAL_PKG" ]]; then
  LOCAL_SRC="$LOCAL_PKG"
elif [[ -n "$OPT_CHANNEL" ]]; then
  : # 命令行已指定渠道
elif [[ $INTERACTIVE -eq 1 ]]; then
  AUTO_CHANNEL="$(resolve_channel "$REPO")"
  printf '\n  安装包来源（自动检测到可用渠道 %s）：\n' "$AUTO_CHANNEL" >&2
  printf '    1) GitHub Release · 自动选择（当前 %s）—— 推荐\n' "$AUTO_CHANNEL" >&2
  printf '    2) GitHub Release · baremetal-latest（main 分支构建）\n' >&2
  printf '    3) 自定义 Release tag\n' >&2
  printf '    4) 本地安装包文件（离线部署）\n' >&2
  choice="$(ask '请选择' '1')"
  case "$choice" in
    1) CHANNEL="$AUTO_CHANNEL" ;;
    2) CHANNEL="baremetal-latest" ;;
    3) CHANNEL="$(ask 'Release tag' "$DEFAULT_CHANNEL")" ;;
    4) LOCAL_SRC="$(ask '本地安装包路径' './aiwork-server-linux-amd64.tar.gz')" ;;
    *) die "无效选择: $choice" ;;
  esac
else
  # 非交互（-y）：未显式指定渠道时自动探测，避免 main 尚未构建时 404
  CHANNEL="$(resolve_channel "$REPO")"
fi

if [[ -z "$LOCAL_SRC" ]]; then
  [[ "$REPO" == */* ]] || die "仓库名需为 owner/repo 形式: $REPO"
  PKG_URL="https://github.com/${REPO}/releases/download/${CHANNEL}/aiwork-server-linux-amd64.tar.gz"
fi

if [[ $UPGRADE -eq 1 ]]; then MODE_DESC="升级（保留数据与密钥）"; else MODE_DESC="全新安装"; fi

cat <<EOF

  即将执行：
    模式        : ${MODE_DESC}
    安装目录    : ${INSTALL_DIR}
    数据目录    : ${DATA_DIR}
    运行用户    : ${RUN_USER}
    监听地址    : 0.0.0.0:${PORT}
    时区        : ${TZ_NAME}
    安装包来源  : ${LOCAL_SRC:-$PKG_URL}
    systemd 单元: ${UNIT_FILE}
    环境变量文件: ${ENV_FILE}

EOF

confirm "确认按以上参数部署？" y || die "已取消"

# ── 2. 获取安装包 ────────────────────────────────────────────────────────────
step 2/8 "获取安装包"

TMP_DIR="$(mktemp -d /tmp/aiwork-deploy.XXXXXX)"
PKG_TGZ="$TMP_DIR/aiwork-server-linux-amd64.tar.gz"

if [[ -n "$LOCAL_SRC" ]]; then
  [[ -f "$LOCAL_SRC" ]] || die "本地安装包不存在: $LOCAL_SRC"
  cp -f "$LOCAL_SRC" "$PKG_TGZ"
  ok "使用本地安装包: $LOCAL_SRC"
else
  say "下载 $PKG_URL"
  curl -fL --retry 3 --retry-delay 2 --connect-timeout 15 \
       -o "$PKG_TGZ" "$PKG_URL" \
    || die "下载失败。若部署机无法访问 GitHub，请手动下载后使用 --local <文件>"
  # 校验和（资产缺失时跳过，不阻断）
  if curl -fsL --connect-timeout 15 -o "$TMP_DIR/pkg.sha256" "${PKG_URL}.sha256" 2>/dev/null; then
    ( cd "$TMP_DIR" && sha256sum -c pkg.sha256 >/dev/null 2>&1 ) \
      || die "安装包校验和不匹配，已中止（可能下载损坏或被篡改）"
    ok "校验和通过"
  else
    warn "未取到 .sha256 校验文件，跳过完整性校验"
  fi
fi

tar -tzf "$PKG_TGZ" >/dev/null 2>&1 || die "安装包不是有效的 tar.gz"
mkdir -p "$TMP_DIR/unpack"
tar -xzf "$PKG_TGZ" -C "$TMP_DIR/unpack"
PKG_ROOT="$TMP_DIR/unpack/aiwork-server-linux-amd64"
[[ -x "$PKG_ROOT/aiwork-server" ]] || die "安装包内缺少 aiwork-server 可执行文件"
[[ -d "$PKG_ROOT/dist" ]]          || die "安装包内缺少前端产物 dist/"
PKG_VERSION="$(cat "$PKG_ROOT/VERSION" 2>/dev/null || echo '未知')"
ok "安装包解包完成（版本 $PKG_VERSION）"

# ── 3. 密钥判定（必须在停服/换二进制之前，中止时服务仍在运行）──────────────────
step 3/8 "密钥判定"

OLD_VAULT="$(env_get AIWORK_VAULT_KEY "$ENV_FILE")"
OLD_ADMIN="$(env_get AIWORK_ADMIN_TOKEN "$ENV_FILE")"

# vault 密钥：三档优先级，任何一档选错都会让已存账号凭据不可解密
#   ① env 里已有 → 沿用（本脚本此前部署过）
#   ② 数据目录已有密钥文件 vault_key.bin → 不注入 env，让程序继续用该文件
#   ③ 全新安装 → 生成随机密钥写入 env
# 特例：存在 vault.stronghold 但没有 vault_key.bin 且 env 无密钥 —— 说明原密钥来源
# 已丢失（如旧 Docker 部署未备份 env），此时程序会新建密钥 → 存量凭据全部不可解密。
# 这属于不可自动修复状态，必须显式提示并由人决定，不静默生成。
VAULT_KEY="$OLD_VAULT"
VAULT_SOURCE="env"
if [[ -z "$VAULT_KEY" ]]; then
  if [[ -f "$DATA_DIR/conf/vault_key.bin" ]]; then
    VAULT_KEY=""
    VAULT_SOURCE="file"
  elif [[ -f "$DATA_DIR/conf/vault.stronghold" ]]; then
    warn "检测到 $DATA_DIR/conf/vault.stronghold（已有加密凭据），但缺少 vault_key.bin，"
    warn "且 $ENV_FILE 中没有 AIWORK_VAULT_KEY —— 原密钥已丢失。"
    warn "继续部署会生成新密钥，导致已存账号凭据全部无法解密（需重新登录导入）。"
    if [[ $FORCE_RESET_VAULT -eq 1 ]]; then
      warn "已指定 --force-reset-vault，按放弃存量凭据继续"
    else
      # -y 也不能静默放行：这条分支会销毁既有数据可用性，必须人工确认或显式传参
      confirm_destructive "确认继续？（原凭据将不可用）" \
        || die "已中止（服务未改动）。请先找回原 AIWORK_VAULT_KEY / vault_key.bin；确认放弃存量凭据请加 --force-reset-vault"
    fi
    VAULT_KEY="$(gen_hex)"
    VAULT_SOURCE="new"
  else
    VAULT_KEY="$(gen_hex)"
    VAULT_SOURCE="new"
  fi
fi

# 管理令牌：env 已有则沿用（运行时 env 优先，降级到文件会静默改变登录令牌）；
# 否则沿用数据目录内的 conf/admin_token；都没有才生成新值。
ADMIN_TOKEN="$OLD_ADMIN"
if [[ -n "$ADMIN_TOKEN" ]]; then
  ADMIN_SOURCE="env"
elif [[ -f "$DATA_DIR/conf/admin_token" ]]; then
  ADMIN_TOKEN=""
  ADMIN_SOURCE="file"
else
  ADMIN_TOKEN="$(gen_hex)"
  ADMIN_SOURCE="new"
fi

# ── 4. 落盘（先停服务，失败可回滚）───────────────────────────────────────────
step 4/8 "安装文件"

if systemctl is-active --quiet "$SERVICE"; then
  say "停止正在运行的 $SERVICE"
  systemctl stop "$SERVICE"
fi

mkdir -p "$INSTALL_DIR"

# 备份上一版用于回滚。dist 与 VERSION 必须一起备份：
# 新二进制启动失败时要还原成「旧二进制 + 旧 dist」，否则回滚后是新旧混装，
# 前后端接口/资源哈希不匹配，UI 反而更不可用
if [[ -f "$INSTALL_DIR/aiwork-server" ]]; then
  cp -f "$INSTALL_DIR/aiwork-server" "$INSTALL_DIR/aiwork-server.prev"
fi
if [[ -d "$INSTALL_DIR/dist" ]]; then
  rm -rf "$INSTALL_DIR/dist.prev"
  cp -a "$INSTALL_DIR/dist" "$INSTALL_DIR/dist.prev"
fi
if [[ -f "$INSTALL_DIR/VERSION" ]]; then
  cp -f "$INSTALL_DIR/VERSION" "$INSTALL_DIR/VERSION.prev"
fi

install -m 0755 "$PKG_ROOT/aiwork-server" "$INSTALL_DIR/aiwork-server"
rm -rf "$INSTALL_DIR/dist"
cp -a "$PKG_ROOT/dist" "$INSTALL_DIR/dist"
printf '%s\n' "$PKG_VERSION" > "$INSTALL_DIR/VERSION"
chown -R root:root "$INSTALL_DIR"
chmod -R a+rX "$INSTALL_DIR"
ok "程序已安装到 $INSTALL_DIR（二进制 + dist）"

# ── 5. 用户与数据目录 ────────────────────────────────────────────────────────
step 5/8 "运行用户与数据目录"

# 显式建同名组：useradd 是否自动建组取决于 /etc/login.defs 的 USERGROUPS_ENAB，
# 后续 chown user:group 依赖该组存在
if ! getent group "$RUN_USER" >/dev/null 2>&1; then
  groupadd --system "$RUN_USER"
fi
if ! id -u "$RUN_USER" >/dev/null 2>&1; then
  useradd --system --gid "$RUN_USER" --home-dir "$DATA_DIR" --shell /usr/sbin/nologin "$RUN_USER"
  ok "已创建系统用户 $RUN_USER"
else
  ok "用户 $RUN_USER 已存在"
fi

mkdir -p "$DATA_DIR"
chown -R "$RUN_USER:$RUN_USER" "$DATA_DIR"
chmod 0750 "$DATA_DIR"
ok "数据目录 $DATA_DIR（属主 $RUN_USER）"

# ── 6. 写环境变量文件（密钥已在第 3 步判定，这里只负责落盘）──────────────────
step 6/8 "写入环境变量"

mkdir -p "$(dirname "$ENV_FILE")"

umask 077
{
  echo "# 由 deploy/install.sh 生成（脚本版本 $SCRIPT_VERSION，$(date '+%F %T')）"
  echo "# 升级请重新执行脚本；手改后注意：AIWORK_VAULT_KEY 变更 = 已存账号凭据不可解密"
  echo "TZ=${TZ_NAME}"
  echo "AIWORK_DATA_DIR=${DATA_DIR}"
  echo "AIWORK_WEB_DIST=${INSTALL_DIR}/dist"
  echo "AIWORK_LISTEN_ADDR=0.0.0.0:${PORT}"
  echo "AIWORK_DISABLE_LOCAL_QUOTA=1"
  # 请求体上限默认 8MiB（clamp [256KiB, 64MiB]）。小内存机可下调省内存；
  # 需要 1.5.4 那种 32MiB 大请求体（长上下文/图片 base64）时取消注释并调大
  echo "#AIWORK_MAX_BODY_BYTES=8388608"
  if [[ -n "$ADMIN_TOKEN" ]]; then echo "AIWORK_ADMIN_TOKEN=${ADMIN_TOKEN}"; fi
  if [[ -n "$VAULT_KEY" ]];   then echo "AIWORK_VAULT_KEY=${VAULT_KEY}";   fi
} > "$ENV_FILE"
umask 022
chmod 0600 "$ENV_FILE"
chown root:root "$ENV_FILE"

case "$VAULT_SOURCE" in
  env)  ok "vault 密钥：沿用 $ENV_FILE 中已有值" ;;
  file) ok "vault 密钥：沿用 $DATA_DIR/conf 内的密钥文件（未注入 env，避免覆盖）" ;;
  new)  ok "vault 密钥：已生成随机值写入 $ENV_FILE" ;;
esac
case "$ADMIN_SOURCE" in
  env)  ok "管理令牌：沿用 $ENV_FILE 中已有值" ;;
  file) ok "管理令牌：沿用 $DATA_DIR/conf/admin_token" ;;
  new)  ok "管理令牌：已生成随机值写入 $ENV_FILE" ;;
esac

# ── 7. systemd 单元 ──────────────────────────────────────────────────────────
step 7/8 "systemd 服务"

# 端口 <1024 需要绑定低端口能力
CAP_LINE=""
if (( PORT < 1024 )); then
  CAP_LINE="AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE"
fi

cat > "$UNIT_FILE" <<EOF
# 由 deploy/install.sh 生成；重新执行脚本会覆盖本文件
[Unit]
Description=AI Work Assistant Server (aiwork-server)
Documentation=https://github.com/${REPO}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${RUN_USER}
WorkingDirectory=${INSTALL_DIR}
EnvironmentFile=${ENV_FILE}
ExecStart=${INSTALL_DIR}/aiwork-server
Restart=always
RestartSec=3
# 应用已注册 SIGTERM 优雅退出（排空用量/计数脏队列后落库）
KillSignal=SIGTERM
TimeoutStopSec=30
${CAP_LINE}
# 加固：程序目录只读，仅数据目录可写
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=${DATA_DIR}

[Install]
WantedBy=multi-user.target
EOF

chmod 0644 "$UNIT_FILE"
systemctl daemon-reload
systemctl enable "$SERVICE" >/dev/null 2>&1
ok "单元已写入并设为开机自启"

# ── 8. 启动与探活（失败自动回滚）─────────────────────────────────────────────
step 8/8 "启动与探活"

wait_health() {
  local i
  for i in $(seq 1 60); do
    if curl -fsS --max-time 2 "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
      return 0
    fi
    # 服务已判定失败则提前退出，不必等满 60s（首次启动含 SQLite/vault 迁移，正常会慢一些）
    if systemctl is-failed --quiet "$SERVICE"; then
      return 1
    fi
    sleep 1
  done
  return 1
}

# 进程存活与 Web UI 可用是两件事：/health 返回常量 JSON、不读 dist，
# 故 dist 缺失/损坏时它仍 200。根路径必须单独校验，否则「部署完成」可能是假成功
check_ui() {
  local body
  body="$(curl -fsS --max-time 3 "http://127.0.0.1:${PORT}/" 2>/dev/null || true)"
  if [[ -z "$body" ]]; then return 1; fi
  # 静态托管缺失时后端会返回这个占位页，说明 dist 没被正确加载
  case "$body" in
    *"Web UI 未构建"*) return 1 ;;
  esac
  return 0
}

# 组合校验：返回 0 通过；1 进程没起来；2 进程在但 UI 不可用
verify_service() {
  if ! wait_health; then return 1; fi
  if ! check_ui; then return 2; fi
  return 0
}

systemctl restart "$SERVICE"

# set -e 下不能直接写 `verify_service; rc=$?`——失败会先退出脚本，取不到状态码
VERIFY_RC=0
verify_service || VERIFY_RC=$?
if [[ $VERIFY_RC -eq 0 ]]; then
  ok "服务已启动，/health 与 Web UI 探活均通过"
else
  if [[ $VERIFY_RC -eq 2 ]]; then
    warn "进程已就绪（/health 通过），但根路径未返回有效页面——dist 可能缺失或未正确加载"
    warn "当前 AIWORK_WEB_DIST=${INSTALL_DIR}/dist，请确认该目录含 index.html"
  else
    warn "服务未能启动（/health 无响应）"
  fi
  warn "最近日志："
  journalctl -u "$SERVICE" -n 30 --no-pager || true
  if [[ -f "$INSTALL_DIR/aiwork-server.prev" ]]; then
    warn "尝试回滚到上一版（二进制 + dist + VERSION）"
    cp -f "$INSTALL_DIR/aiwork-server.prev" "$INSTALL_DIR/aiwork-server"
    if [[ -d "$INSTALL_DIR/dist.prev" ]]; then
      rm -rf "$INSTALL_DIR/dist"
      cp -a "$INSTALL_DIR/dist.prev" "$INSTALL_DIR/dist"
    fi
    if [[ -f "$INSTALL_DIR/VERSION.prev" ]]; then
      cp -f "$INSTALL_DIR/VERSION.prev" "$INSTALL_DIR/VERSION"
    fi
    systemctl restart "$SERVICE"
    if verify_service; then
      die "新版本未通过探活，已完整回滚到旧版本并恢复运行。请检查新版本兼容性后重试"
    fi
  fi
  die "服务启动失败，请查看：journalctl -u ${SERVICE} -n 100 --no-pager"
fi

# ── 汇总 ─────────────────────────────────────────────────────────────────────
IP_HINT="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
if [[ -z "${IP_HINT:-}" ]]; then IP_HINT="<服务器IP>"; fi

# 令牌展示：env 注入则用 env 值，否则读数据目录内的文件
if [[ "$ADMIN_SOURCE" == "file" ]]; then
  SHOW_TOKEN="$(tr -d '\r\n' < "$DATA_DIR/conf/admin_token" 2>/dev/null || true)"
else
  SHOW_TOKEN="$ADMIN_TOKEN"
fi

cat <<EOF

${c_ok}════════════════════ 部署完成 ════════════════════${c_off}

  版本        : ${PKG_VERSION}
  访问地址    : http://${IP_HINT}:${PORT}
  管理令牌    : ${SHOW_TOKEN}
  数据目录    : ${DATA_DIR}
  环境变量    : ${ENV_FILE}
  服务管理    : systemctl {status|restart|stop} ${SERVICE}
  实时日志    : journalctl -u ${SERVICE} -f

  下一步：
    1) 浏览器打开上面的地址，粘贴管理令牌登录；
    2) 服务监听 0.0.0.0:${PORT}（非环回）——「关闭鉴权」开关不生效，
       /v1/* 网关必须至少启用一个 API Key，/api/* 仍用管理员令牌登录；
    3) 公网部署请在前面加 TLS 反向代理，并把端口映射改为仅本机可达。

  备份（密钥丢失 = 账号凭据不可解密，数据目录与 env 必须一起备份）：
    systemctl stop ${SERVICE}
    tar czf ~/aiwork-$(date +%F).tgz -C ${DATA_DIR%/*} ${DATA_DIR##*/} ${ENV_FILE}
    systemctl start ${SERVICE}

  升级：重新执行本次使用的同一条命令即可（数据与密钥原样保留）
    curl -fsSL https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/install.sh | sudo bash
    # 若已把脚本下载到本机，也可直接：sudo bash install.sh

EOF
