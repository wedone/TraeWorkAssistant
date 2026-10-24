# 🚀 Web 版服务端部署指南

> 单进程 axum 服务（`aiwork-server`）：管理面 REST + OpenAI/Anthropic 兼容网关 + 7 项定时任务调度 + 三渠道通知推送（Bark/Server酱/Webhook，应用内配置）+ React 静态托管，浏览器任意设备直访。

## 📋 快速开始

### 方式一：裸机一键部署（Debian 12，推荐内网服务器）

无需 Docker，也无需在部署机上装 Rust/Node——二进制与前端产物由 CI 构建并发布到 Release。
脚本自包含，**不需要克隆仓库**，一行命令即可：

```bash
curl -fsSL https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/install.sh | sudo bash
```

需要传参时用 `-s --` 把参数交给脚本（管道下直接跟参数会被 `bash` 当成自己的参数）：

```bash
curl -fsSL https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/install.sh \
  | sudo bash -s -- -y
```

也可以先下载再执行，便于审查内容：

```bash
curl -fsSLO https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/install.sh
sudo bash install.sh
```

若部署机无法访问 GitHub（raw 与 Release 都取不到），先在能出网的机器上下载安装包，再用 `--local`：

```bash
sudo bash install.sh --local ./aiwork-server-linux-amd64.tar.gz
```

脚本会依次完成：环境检查 → 下载 CI 产物（校验 sha256）→ 安装到 `/opt/aiwork` → 创建系统用户
`aiwork` → 初始化 `/var/lib/aiwork` → 生成 `/etc/aiwork/aiwork.env`（含随机管理令牌与 vault 密钥）
→ 写入 systemd 单元 → 启动并探活 → 打印访问地址与令牌。

常用变体：

```bash
sudo bash install.sh -y                      # 全默认、零交互
sudo bash install.sh --local ./pkg.tar.gz    # 离线部署（部署机不能出网）
sudo bash install.sh --port 9090 --data-dir /srv/aiwork
sudo bash install.sh --channel baremetal-docker_main   # 用 docker_main 分支构建
```

交互式会依次询问：安装目录、数据目录、运行用户、端口、时区、安装包来源（默认自动选择可用渠道）。
管道方式（`curl | sudo bash`）同样能交互——脚本会从 `/dev/tty` 读取，不会被脚本内容干扰。

重复执行即升级：数据目录与密钥原样保留，新版本启动或探活失败会自动完整回滚上一版
（二进制 + dist + VERSION 一起还原）。探活同时校验 `/health` 与根路径，避免 dist 缺失时
出现「部署成功但页面打不开」的假成功。

`--channel` 不指定时脚本会自动探测：优先 `baremetal-latest`（main 分支构建），若尚未有该渠道
则回落到最新的 `baremetal-<分支名>`，因此在还没合并到 main 时也能直接部署。

> 用 `main` 分支的 raw 地址取脚本，但 CI 只产出 `baremetal-docker_main` 渠道时，
> 脚本会自动回落到该渠道，无需改 URL。反之若想固定用某分支的脚本内容，
> 把 URL 里的 `main` 换成对应分支名即可。

**部署机要求**：Debian 12 / x86_64 / systemd。脚本只会补装 `curl`、`tar`、`tzdata`、
`ca-certificates`（已存在则跳过），不安装 Docker、不安装编译工具链。

### 卸载旧实例

若机器上已有旧版（Docker 或裸机），用 `deploy/uninstall.sh` 清理。它默认只做 dry-run 探测，
列出将被删除的内容，不做任何修改：

```bash
# 1. 先看会删什么（dry-run，安全）
curl -fsSLO https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/uninstall.sh
sudo bash uninstall.sh

# 2. 确认无误后执行（连数据一起删）
sudo bash uninstall.sh --yes

# 3. 或保留数据（SQLite / vault 密钥 / conf / logs）
sudo bash uninstall.sh --yes --keep-data
```

自动识别两种形态：Docker（容器 `aiwork-server`、镜像 `aiwork-server:*`、
`ghcr.io/*/traeworkassistant`、含 `aiwork-server` 服务的 compose 项目及其绑定挂载目录）
与裸机（`aiwork-server.service`、`/opt/aiwork`、`/var/lib/aiwork`、`/etc/aiwork`）。

删除范围受白名单约束：只删上述标准路径，以及位于已识别 compose/程序目录之下的数据目录；
其他位置的数据目录会跳过并提示。**源码仓库目录只列出不删**（避免误删未提交内容），
需要时按脚本末尾给出的 `rm -rf` 命令手动处理。

> ⚠️ 不加 `--keep-data` 会连同 vault 密钥一起删除，已存账号凭据永久不可解密。

**为什么不用 Docker 也能跑**：镜像里那层 Docker 特化只是环境变量
（`AIWORK_DATA_DIR` / `AIWORK_WEB_DIST` / `AIWORK_LISTEN_ADDR` / `AIWORK_DISABLE_LOCAL_QUOTA`），
脚本用 `EnvironmentFile=/etc/aiwork/aiwork.env` 注入等价取值，代码零改动。

产物在 `debian:bookworm-slim` 里构建，glibc 版本与 Debian 12（2.36）一致；在更新发行版上编译再
拷回 Debian 12 会因 glibc 过新而无法启动——这也是直接用 CI 产物而非本机编译的原因。

### 方式二：Docker

前置：Docker 20.10+ 与 Docker Compose v2。

```bash
# 1. 构建并启动（首次构建约 10-20 分钟，Rust 编译占大头）
docker compose up -d --build

# 2. 查看健康状态（HEALTHCHECK 30s 间隔，等待变 healthy）
docker compose ps

# 3. 获取管理令牌（未注入 AIWORK_ADMIN_TOKEN 时自动生成）
docker compose exec aiwork-server cat /app/data/conf/admin_token

# 4. 浏览器访问 http://<服务器IP>:8080 ，粘贴令牌登录
```

宿主端口自定义：项目根目录建 `.env` 写入 `AIWORK_PORT=9090` 后重建容器。

## 🔧 配置项（环境变量）

| 变量 | 默认值 | 说明 |
|---|---|---|
| `AIWORK_LISTEN_ADDR` | `0.0.0.0:8080` | 监听地址；改端口时需同步 `AIWORK_PORT` 与 compose 端口映射 |
| `AIWORK_PORT` | `8080` | 仅供容器 HEALTHCHECK 探活取端口 |
| `AIWORK_ADMIN_TOKEN` | 空（自动生成） | 管理面令牌；留空则首启生成 64 位随机 hex 写 `conf/admin_token` |
| `AIWORK_VAULT_KEY` | 空（自动生成） | 敏感数据加密密钥（任意字符串 SHA-256 归一 32B）；**容器重建必须可复现**，否则已存凭据不可解密 |
| `AIWORK_DATA_DIR` | `/app/data` | 数据目录（容器内固定挂载 volume） |
| `AIWORK_WEB_DIST` | `/app/dist` | 前端静态资源目录（镜像内置） |
| `TZ` | `Asia/Shanghai` | 时区（影响签到/调度的时间判定） |

## 💾 数据持久化（volume `./data:/app/data`）

| 子目录 | 内容 | 备份要点 |
|---|---|---|
| `data/` | SQLite 库（账号/分组/签到记录/积分快照/用量统计） | 常规备份 |
| `conf/` | `admin_token`、`vault_key.bin` | **必须备份**：`vault_key.bin` 丢失 = 全部账号凭据不可解密 |
| `logs/` | 运行日志（按保留天数自动清理） | 可不备份 |

升级流程：`git pull && docker compose up -d --build`（数据在 volume，升级不丢）。

## 🔑 OAuth 账号录入（无桌面参与）

### Trae（粘贴回调模式）

1. 账号管理页 → OAuth 登录 → 复制登录链接，在**任意设备**浏览器打开并完成 Trae 登录；
2. 登录成功后浏览器跳转 `http://127.0.0.1:17388/authorize?...` 并停在「无法连接」页——**这是预期现象**；
3. 复制浏览器地址栏完整 URL，粘贴回 Web UI 提交，账号自动入池。

### WorkBuddy（authUrl 轮询）

Buddy 账号页发起 OAuth 后按提示在浏览器完成授权，服务端自动轮询收取 token（≤300s），无需粘贴。

## 📦 桌面版数据迁移

1. 桌面版 → 账号管理 → 导出 JSON（含明文 JWT/refresh_token）；
2. Web 版 → 账号管理 → 导入（按 uid 去重、分组合并；导入前可预览）；
3. WorkBuddy 账号同理（Buddy 账号页导入导出）。

> ⚠️ 桌面版 vault 快照（DPAPI 加密）**不可迁移**——凭据经导出文件以明文中转，导出后请及时删除该文件。

## 🛡️ 安全边界

- **管理面** `/api/*`：主令牌 + 附加管理员令牌（可签发/吊销，最多 20 个）并集校验 → HttpOnly Cookie 会话（7 天有效期），未登录一律 401；
- **网关** `/v1/*`：api_keys 鉴权 fail-closed（未配置 key 即全拒）；
- **网关 API Key vault 加密落盘**（v1.4.1 起）：API Key 不再明文存 SQLite——真值加密写入 vault（`data/conf/vault.stronghold`），库中仅占位；主密码优先取环境变量 `AIWORK_VAULT_KEY`（跨机/容器重建可复现），否则自动生成 `data/conf/vault_key.bin`。**两个文件都在 data 卷内，备份 data 卷时需一并保留**；升级首启自动迁移存量明文 key（幂等，失败时明文保留下次重试）；
- **「关闭鉴权」开关仅环回监听生效**（v1.4.1 起）：容器/compose 默认监听 `0.0.0.0:8080`（非环回），此时 `auth_disabled` 不生效——曾以「无启用 Key + 关闭鉴权」模式运行的存量部署，升级后 `/v1/*` 将 401，请在 Web 端 API 服务页启用至少一个 API Key；
- **`/health`**：公开探活端点，仅输出聚合级运营汇总（池计数 / 积分合计 / 今日 token 合计，无账号级明细与凭据）；
- 出网：服务端以合成指纹直连 `api.trae.cn` / `trae-api-cn.mchost.guru` / `copilot.tencent.com`，**不读系统代理**；
- 建议加固（公网部署必做）：反向代理加 TLS（见下节）、管理面 IP 允许列表（反代层或应用层内置，见下节）、`AIWORK_ADMIN_TOKEN` 显式注入强随机值、禁用 8080 直接对公网暴露（compose 端口映射改 `127.0.0.1:8080:8080` 仅本机反代可达）。

### 🔒 TLS 反向代理示例

Caddy（自动 HTTPS，最省事）：

```caddy
# Caddyfile
aiwork.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

Nginx（含 SSE/WebSocket 关键配置：WS 升级头 + 关闭缓冲 + 长超时，否则实时进度推送不可用）：

```nginx
server {
    listen 443 ssl;
    server_name aiwork.example.com;
    # ssl_certificate / ssl_certificate_key 按需配置（或用 certbot 自动签）

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        # WebSocket 必需：/api/ws 实时事件通道（缺升级头时前端自动回退 SSE，建议显式开启）
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        # SSE 必需：/api/events/* 禁缓冲、不超时断流
        proxy_buffering off;
        proxy_read_timeout 3600s;
    }
}
```

### 🚫 管理面 IP 允许列表（反代层）

Nginx 在 `location /` 内加：

```nginx
    allow 203.0.113.0/24;   # 办公网段
    allow 198.51.100.7;     # 个人固定 IP
    deny  all;
```

Caddy 用 `@blocked not remote_ip ...` + `respond @blocked 403` 等价实现；云服务器也可直接在安全组限制 443 来源 IP。

### 🛡️ 管理面 IP 允许列表（应用层，服务端内置）

除反代层外，服务端内置应用层 IP 允许列表（设置页「IP 允许列表」卡，kv 热生效）：

- **CIDR 白名单**：每行一条（如 `203.0.113.0/24`、`198.51.100.7`），支持 IPv4/IPv6；保存即时生效，含无效条目整体拒绝不落库；
- **trust_proxy**：经反向代理部署时**必须勾选**，此时取 `X-Real-IP`（或 `X-Forwarded-For` 首项）判定真实来源——上文 Nginx 示例已配置该头；不勾选则取 TCP 对端地址（即反代自身 IP，会导致全放行或全拒绝）；
- **回环豁免**：`127.0.0.1` / `::1` 始终放行，避免误配置锁死本机运维；`/health` 探活不受限；
- 未启用或列表为空时不拦截；取不到来源 IP 时 fail-closed 拒绝（403）。

反代层与应用层可叠加：反代层挡流量，应用层兜底直连 8080 的旁路访问。

## 💾 备份与恢复

```bash
# 备份（停机一致性最好，运行中备份 SQLite 建议用 sqlite3 .backup）
tar czf aiwork-backup-$(date +%F).tgz data/

# 恢复：解压回原路径后 docker compose up -d
# 关键文件：data/*.db（业务库）、conf/vault_key.bin（加密密钥）、conf/admin_token
```

> ⚠️ `vault_key.bin` 丢失 = 已存账号凭据永久不可解密（无找回手段）。跨机迁移要么携带该文件，要么两侧固定相同 `AIWORK_VAULT_KEY`。

### 裸机部署（install.sh）的备份与升级

裸机部署的数据目录默认 `/var/lib/aiwork`，密钥默认以 `AIWORK_VAULT_KEY` 形式写在
`/etc/aiwork/aiwork.env`（权限 0600，仅 root 可读）。

```bash
# 备份（业务库 + 密钥文件一起打包；env 文件另存一份）
systemctl stop aiwork-server
tar czf ~/aiwork-$(date +%F).tgz -C /var/lib aiwork
cp /etc/aiwork/aiwork.env ~/aiwork-env-$(date +%F).bak
systemctl start aiwork-server

# 升级：重新执行安装时用的同一条命令即可
# （自动停服 → 换二进制与 dist → 重启 → 探活，失败完整回滚上一版）
curl -fsSL https://raw.githubusercontent.com/wedone/TraeWorkAssistant/main/deploy/install.sh \
  | sudo bash -s -- -y
```

> ⚠️ 裸机部署的 vault 密钥在 `/etc/aiwork/aiwork.env` 里，**不在**数据目录内。
> 只备份 `/var/lib/aiwork` 会丢掉密钥——必须连同 env 文件一起备份。
> 脚本重跑时会沿用已有 env 中的密钥；若 env 丢失但 `conf/vault.stronghold` 仍在，
> 脚本会在停服之前检测到这一状态并中止，避免静默生成新密钥把存量凭据变成不可解密。
> 确认要放弃存量凭据才继续时，显式加 `--force-reset-vault`（`-y` 不会自动放行这条）。

## 🩺 常见问题

| 现象 | 处置 |
|---|---|
| 容器 restart 循环 | `docker compose logs aiwork-server` 看启动错误；多为 `AIWORK_VAULT_KEY` 与已有 `vault_key.bin` 不一致 |
| 登录提示 token 无效 | 令牌取自 `conf/admin_token` 文件或 env 注入值；env 变更后需 `docker compose up -d` 重建；附加管理员令牌（设置页签发）同样可登录，吊销后立即失效 |
| 签到时间不对 | 确认 `TZ=Asia/Shanghai` 未被覆盖 |
| 手动签到无进度 | 实时通道走 WebSocket `/api/ws`（连接失败前端自动回退 SSE `/api/events/checkin`）；Nginx 需带 Upgrade 升级头并对 SSE `proxy_buffering off`（见上节示例） |
| 定时任务没跑 | 任务由服务端内置调度器托管（05:30 Trae JWT 续期 / 05:40 模型列表同步 / 09:00 Trae 签到 / 09:10 WB 签到 / 10:30 WB 续期 / 23:30、23:40 积分快照），重启后自动补跑当天错过的任务；确认环境配置页对应任务开关未停用 |
