#!/usr/bin/env bash
# =============================================================================
# AI Work 助手（TraeWorkAssistant）卸载脚本 —— Debian 12
#
# 用法：
#   sudo bash uninstall.sh                 # 只探测并列出将被删除的内容（dry-run，默认）
#   sudo bash uninstall.sh --yes           # 确认执行卸载（连数据一起删）
#   sudo bash uninstall.sh --yes --keep-data   # 卸载程序但保留数据目录
#
# 自动识别两种部署形态（可同时存在）：
#   A. Docker / docker compose：容器 aiwork-server、镜像 aiwork-server:latest /
#      ghcr.io/*/traeworkassistant、绑定挂载的 ./data
#   B. 裸机 systemd：aiwork-server.service + /opt/aiwork + /var/lib/aiwork + /etc/aiwork
#
# 安全约定：
#   - 默认 dry-run，不做任何修改；只有显式 --yes 才真正删除
#   - 只删白名单路径，且每条路径删除前都会打印
#   - 源码仓库目录（如 /root/TraeWorkAssistant）只列出、不自动删，
#     避免误删未提交内容；脚本会提示你手动处理
# =============================================================================

set -Eeuo pipefail

ASSUME_YES=0
KEEP_DATA=0

# 用 $'...' 让变量里存真实 ESC 字符：printf 只解释格式串的转义，
# 以 %s 传参时不会解析字面的 \033
c_info=$'\033[1;36m'; c_ok=$'\033[1;32m'; c_warn=$'\033[1;33m'; c_err=$'\033[1;31m'; c_off=$'\033[0m'
say()  { printf "${c_info}==>${c_off} %s\n" "$*"; }
ok()   { printf "${c_ok}  ✓${c_off} %s\n" "$*"; }
warn() { printf "${c_warn}  !${c_off} %s\n" "$*" >&2; }
die()  { printf "${c_err}  ✗${c_off} %s\n" "$*" >&2; exit 1; }
step() { printf "\n${c_info}[%s]${c_off} %s\n" "$1" "$2"; }

usage() {
  cat <<'EOF'
用法: uninstall.sh [选项]

  (无参数)               dry-run：只列出将被删除的内容，不做修改
  -y, --yes              确认执行卸载（连数据一起删）
      --keep-data        保留数据目录（SQLite / vault 密钥 / conf / logs）
  -h, --help             显示本帮助

数据目录含 vault 密钥，删除后已存账号凭据永久不可解密。需要保留请加 --keep-data。
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -y|--yes)    ASSUME_YES=1; shift ;;
    --dry-run)   ASSUME_YES=0; shift ;;
    --keep-data) KEEP_DATA=1; shift ;;
    -h|--help)   usage; exit 0 ;;
    *)           die "未知参数: $1（--help 查看用法）" ;;
  esac
done

[[ $EUID -eq 0 ]] || die "需要 root 权限：请在命令前加 sudo"

# ── 探测结果容器 ─────────────────────────────────────────────────────────────
CONTAINERS=()      # 容器名
IMAGES=()          # 镜像引用
COMPOSE_DIRS=()    # 含 aiwork-server 服务的 compose 文件所在目录
UNITS=()           # systemd 单元名
PROG_DIRS=()       # 程序目录（可整目录删除）
DATA_DIRS=()       # 数据目录
CONF_DIRS=()       # 配置目录（env 文件等）
REPO_DIRS=()       # 源码仓库（只提示，不删）
USERS=()           # 服务账号

# 去重追加：用 nameref 而非 eval，避免路径含空格/特殊字符时被二次解析
add_unique() { # add_unique <数组名> <值>
  local -n _arr="$1"
  local val="$2" x
  [[ -z "$val" ]] && return 0
  for x in "${_arr[@]:-}"; do
    [[ "$x" == "$val" ]] && return 0
  done
  _arr+=("$val")
}

# ── 1. 探测 Docker 形态 ──────────────────────────────────────────────────────
step 1/4 "探测 Docker 部署"

if command -v docker >/dev/null 2>&1; then
  # 容器：按名字与镜像两个维度找，兼容改过 container_name 的情况
  while IFS=$'\t' read -r cname cimage; do
    [[ -z "$cname" ]] && continue
    case "$cname $cimage" in
      *aiwork*|*AIWork*|*TraeWorkAssistant*|*traeworkassistant*)
        add_unique CONTAINERS "$cname"
        add_unique IMAGES "$cimage"
        ;;
    esac
  done < <(docker ps -a --format '{{.Names}}\t{{.Image}}' 2>/dev/null || true)

  # 镜像：本地 tag 里带 aiwork / traeworkassistant 的
  while IFS= read -r img; do
    [[ -z "$img" ]] && continue
    add_unique IMAGES "$img"
  done < <(docker images --format '{{.Repository}}:{{.Tag}}' 2>/dev/null \
           | grep -Ei 'aiwork|traeworkassistant' | grep -v '<none>' || true)

  # compose 项目：在常见根目录下找含 aiwork-server 的 compose 文件
  for base in /root /home /opt /srv /data /var/www /mnt /usr/local; do
    [[ -d "$base" ]] || continue
    while IFS= read -r f; do
      [[ -z "$f" ]] && continue
      if grep -qE '^\s*aiwork-server\s*:' "$f" 2>/dev/null; then
        add_unique COMPOSE_DIRS "$(dirname "$f")"
      fi
    done < <(find "$base" -maxdepth 4 -type f \( -name 'docker-compose.yml' -o -name 'docker-compose.yaml' -o -name 'compose.yml' -o -name 'compose.yaml' \) 2>/dev/null || true)
  done

  # 容器绑定的宿主机目录（如 ./data → /opt/xxx/data）
  for c in "${CONTAINERS[@]:-}"; do
    [[ -z "$c" ]] && continue
    while IFS= read -r src; do
      [[ -z "$src" ]] && continue
      add_unique DATA_DIRS "$src"
    done < <(docker inspect -f '{{range .Mounts}}{{if eq .Type "bind"}}{{.Source}}{{"\n"}}{{end}}{{end}}' "$c" 2>/dev/null || true)
  done
else
  ok "未安装 Docker，跳过"
fi

if [[ ${#CONTAINERS[@]} -eq 0 && ${#COMPOSE_DIRS[@]} -eq 0 && ${#IMAGES[@]} -eq 0 ]]; then
  ok "未发现 Docker 形态的 aiwork-server"
fi

# ── 2. 探测裸机 systemd 形态 ─────────────────────────────────────────────────
step 2/4 "探测裸机 systemd 部署"

if [[ -f /etc/systemd/system/aiwork-server.service ]]; then
  add_unique UNITS "aiwork-server"
fi
# 其他可能命名的单元。统一去掉 .service 后缀存储，避免与上面的裸名重复、
# 也避免删除时拼成 aiwork-server.service.service
while IFS= read -r u; do
  [[ -z "$u" ]] && continue
  add_unique UNITS "${u%.service}"
done < <(systemctl list-unit-files --no-legend --no-pager 2>/dev/null \
         | awk '{print $1}' | grep -Ei '^(aiwork|traeworkassistant).*\.service$' || true)

for d in /opt/aiwork /usr/local/aiwork /opt/TraeWorkAssistant /usr/local/TraeWorkAssistant; do
  [[ -d "$d" ]] && add_unique PROG_DIRS "$d"
done
for d in /var/lib/aiwork /data/AIWorkAssistant /var/lib/TraeWorkAssistant; do
  [[ -d "$d" ]] && add_unique DATA_DIRS "$d"
done
for d in /etc/aiwork; do
  [[ -d "$d" ]] && add_unique CONF_DIRS "$d"
done

if id -u aiwork >/dev/null 2>&1; then
  add_unique USERS "aiwork"
fi

if [[ ${#UNITS[@]} -eq 0 && ${#PROG_DIRS[@]} -eq 0 ]]; then
  ok "未发现 systemd 形态的 aiwork-server"
fi

# ── 3. 源码仓库目录（只列出，不删）───────────────────────────────────────────
step 3/4 "源码仓库目录（仅提示，不自动删除）"

for base in /root /home /opt /srv /data /mnt /var/www; do
  [[ -d "$base" ]] || continue
  while IFS= read -r d; do
    [[ -z "$d" ]] && continue
    add_unique REPO_DIRS "$d"
  done < <(find "$base" -maxdepth 3 -type d \( -name 'TraeWorkAssistant' -o -name 'traeworkassistant' \) 2>/dev/null || true)
done

# ── 4. 汇总 ─────────────────────────────────────────────────────────────────
step 4/4 "待处理清单"

print_list() {
  local title="$1"; shift
  local arr=("$@")
  # 数组为空时 "${arr[@]:-}" 会展开成一个空串参数，需先过滤掉
  local -a real=()
  local x
  for x in "${arr[@]:-}"; do [[ -n "$x" ]] && real+=("$x"); done
  if [[ ${#real[@]} -eq 0 ]]; then
    printf '  %-14s （无）\n' "$title"
  else
    printf '  %-14s\n' "$title"
    for x in "${real[@]}"; do printf '      - %s\n' "$x"; done
  fi
  # 末尾显式 return 0：否则循环体里最后一个 [[ -n ]] 为假时函数返回 1，
  # 在 set -e 下会把整个脚本中断
  return 0
}

print_list "容器:"     "${CONTAINERS[@]:-}"
print_list "镜像:"     "${IMAGES[@]:-}"
print_list "compose 目录:" "${COMPOSE_DIRS[@]:-}"
print_list "systemd 单元:" "${UNITS[@]:-}"
print_list "程序目录:"  "${PROG_DIRS[@]:-}"
if [[ $KEEP_DATA -eq 1 ]]; then
  print_list "数据目录(保留):" "${DATA_DIRS[@]:-}"
else
  print_list "数据目录(删除):" "${DATA_DIRS[@]:-}"
fi
print_list "配置目录:"  "${CONF_DIRS[@]:-}"
print_list "服务账号:"  "${USERS[@]:-}"
print_list "源码仓库(不删):" "${REPO_DIRS[@]:-}"

NOTHING=1
if [[ ${#CONTAINERS[@]} -gt 0 || ${#IMAGES[@]} -gt 0 || ${#COMPOSE_DIRS[@]} -gt 0 \
      || ${#UNITS[@]} -gt 0 || ${#PROG_DIRS[@]} -gt 0 || ${#CONF_DIRS[@]} -gt 0 \
      || ${#USERS[@]} -gt 0 ]]; then
  NOTHING=0
fi
if [[ $KEEP_DATA -eq 0 && ${#DATA_DIRS[@]} -gt 0 ]]; then
  NOTHING=0
fi

if [[ $NOTHING -eq 1 ]]; then
  say "未发现任何需要卸载的实例"
  exit 0
fi

if [[ $ASSUME_YES -ne 1 ]]; then
  # 用 printf 而非 heredoc：heredoc 不解释 \033 转义，颜色码会被原样打印
  printf '\n%s当前为 dry-run，未做任何修改。%s\n' "$c_warn" "$c_off"
  printf '确认执行卸载请重跑并加 --yes：\n'
  printf '  sudo bash uninstall.sh --yes%s\n\n' "$( [[ $KEEP_DATA -eq 1 ]] && echo ' --keep-data' )"
  exit 0
fi

if [[ $KEEP_DATA -eq 0 && ${#DATA_DIRS[@]} -gt 0 ]]; then
  warn "将删除以下数据目录（含 vault 密钥，删除后已存账号凭据永久不可解密）："
  for d in "${DATA_DIRS[@]}"; do [[ -n "$d" ]] && warn "    $d"; done
fi

# ── 执行卸载 ────────────────────────────────────────────────────────────────
step "执行" "开始卸载"

# 1) systemd 服务
for u in "${UNITS[@]:-}"; do
  [[ -z "$u" ]] && continue
  # 只要单元文件存在就尝试停止；服务未运行或不存在时 stop 会失败，用 || true 兜住
  if [[ -f "/etc/systemd/system/$u.service" ]] \
     || systemctl list-unit-files --no-legend --no-pager "$u.service" 2>/dev/null | grep -q "^$u.service"; then
    systemctl stop "$u" 2>/dev/null || true
    systemctl disable "$u" 2>/dev/null || true
    ok "已停止并禁用 $u"
  fi
  if [[ -f "/etc/systemd/system/$u.service" ]]; then
    if rm -f "/etc/systemd/system/$u.service"; then
      ok "已删除单元文件 $u.service"
    else
      warn "删除单元文件 $u.service 失败（可能需要 root）"
    fi
  fi
done
if [[ ${#UNITS[@]} -gt 0 ]]; then
  systemctl daemon-reload
  systemctl reset-failed 2>/dev/null || true
fi

# 2) compose 项目（优先用 compose down，能一并清理网络）
for d in "${COMPOSE_DIRS[@]:-}"; do
  [[ -z "$d" ]] && continue
  if [[ -f "$d/docker-compose.yml" || -f "$d/docker-compose.yaml" || -f "$d/compose.yml" || -f "$d/compose.yaml" ]]; then
    if ( cd "$d" && docker compose down --rmi all --remove-orphans ) >/dev/null 2>&1 \
       || ( cd "$d" && docker-compose down --rmi all --remove-orphans ) >/dev/null 2>&1; then
      ok "已停止 compose 项目 $d"
    else
      warn "compose down 失败，继续按容器/镜像逐个清理"
    fi
  fi
done

# 3) 残留容器
for c in "${CONTAINERS[@]:-}"; do
  [[ -z "$c" ]] && continue
  docker rm -f "$c" >/dev/null 2>&1 && ok "已删除容器 $c" || warn "删除容器 $c 失败"
done

# 4) 残留镜像
for i in "${IMAGES[@]:-}"; do
  [[ -z "$i" ]] && continue
  docker rmi -f "$i" >/dev/null 2>&1 && ok "已删除镜像 $i" || warn "删除镜像 $i 失败（可能被其他容器占用）"
done

# 5) 程序目录
for d in "${PROG_DIRS[@]:-}"; do
  [[ -z "$d" ]] && continue
  case "$d" in
    /opt/aiwork|/usr/local/aiwork|/opt/TraeWorkAssistant|/usr/local/TraeWorkAssistant)
      rm -rf "$d" && ok "已删除程序目录 $d" ;;
    *) warn "跳过非白名单程序目录 $d" ;;
  esac
done

# 6) 配置目录
for d in "${CONF_DIRS[@]:-}"; do
  [[ -z "$d" ]] && continue
  if [[ "$d" == "/etc/aiwork" ]]; then
    if rm -rf "$d"; then ok "已删除配置目录 $d"; else warn "删除配置目录 $d 失败"; fi
  fi
done

# 7) 数据目录（--keep-data 时保留）
if [[ $KEEP_DATA -eq 0 ]]; then
  for d in "${DATA_DIRS[@]:-}"; do
    [[ -z "$d" ]] && continue
    # 白名单：标准数据目录，或位于已识别的 compose/程序目录之下
    case "$d" in
      /var/lib/aiwork|/data/AIWorkAssistant|/var/lib/TraeWorkAssistant)
        if rm -rf "$d"; then ok "已删除数据目录 $d"; else warn "删除数据目录 $d 失败"; fi ;;
      *)
        ALLOW=0
        for p in "${COMPOSE_DIRS[@]:-}" "${PROG_DIRS[@]:-}"; do
          [[ -n "$p" && "$d" == "$p"/* ]] && ALLOW=1
        done
        if [[ $ALLOW -eq 1 ]]; then
          if rm -rf "$d"; then ok "已删除数据目录 $d"; else warn "删除数据目录 $d 失败"; fi
        else
          warn "跳过非白名单数据目录 $d（请确认后手动删除）"
        fi
        ;;
    esac
  done
else
  for d in "${DATA_DIRS[@]:-}"; do
    [[ -z "$d" ]] && continue
    ok "保留数据目录 $d"
  done
fi

# 8) 服务账号
for u in "${USERS[@]:-}"; do
  [[ -z "$u" ]] && continue
  if [[ "$u" == "aiwork" ]] && id -u aiwork >/dev/null 2>&1; then
    userdel aiwork 2>/dev/null && ok "已删除服务账号 aiwork" || warn "删除账号 aiwork 失败"
    getent group aiwork >/dev/null 2>&1 && groupdel aiwork 2>/dev/null || true
  fi
done

# ── 汇总 ─────────────────────────────────────────────────────────────────────
cat <<EOF

${c_ok}════════════════════ 卸载完成 ════════════════════${c_off}

  已清理 Docker 容器/镜像/compose 项目、systemd 单元、程序与配置目录。
$( if [[ $KEEP_DATA -eq 0 ]]; then echo "  数据目录已一并删除（含 vault 密钥，原凭据不可恢复）。"; else echo "  数据目录已保留，可在重新部署后继续使用。"; fi )

  源码仓库目录未自动删除，如不需要请手动处理：
$( for d in "${REPO_DIRS[@]:-}"; do [[ -n "$d" ]] && echo "    rm -rf $d"; done; if [[ ${#REPO_DIRS[@]} -eq 0 ]]; then echo "    （无）"; fi )

  重新部署：
    curl -fsSL https://raw.githubusercontent.com/wedone/TraeWorkAssistant/docker_main/deploy/install.sh | sudo bash

EOF
