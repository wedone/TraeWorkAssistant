# 🚀 Web 版服务端部署指南

> 单进程 axum 服务（`aiwork-server`）：管理面 REST + OpenAI/Anthropic 兼容网关 + 7 项定时任务调度 + 三渠道通知推送（Bark/Server酱/Webhook，应用内配置）+ React 静态托管，浏览器任意设备直访。

## 📋 快速开始（Docker）

前置：Docker 20.10+ 与 Docker Compose v2。

> 不想用 Docker？见下文「Debian 原生部署（systemd）」，单二进制直接由 systemd 托管。

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
| `AIWORK_LISTEN_ADDR` | Docker `0.0.0.0:8080` / 原生 `127.0.0.1:8080` | 监听地址；Docker 改端口时需同步 `AIWORK_PORT` 与 compose 端口映射 |
| `AIWORK_PORT` | `8080` | 仅供容器 HEALTHCHECK 探活取端口；原生部署不需要 |
| `AIWORK_ADMIN_TOKEN` | 空（自动生成） | 管理面令牌；留空则首启生成 64 位随机 hex 写 `conf/admin_token` |
| `AIWORK_VAULT_KEY` | 空（自动生成） | 敏感数据加密密钥（任意字符串 SHA-256 归一 32B）；**容器重建必须可复现**，否则已存凭据不可解密 |
| `AIWORK_DATA_DIR` | Docker `/app/data` / 原生 `/var/lib/aiwork` | 数据目录 |
| `AIWORK_WEB_DIST` | Docker `/app/dist` / 原生 `/opt/aiwork/dist` | 前端静态资源目录 |
| `AIWORK_DISABLE_LOCAL_QUOTA` | 空 | 服务端无本机 WorkBuddy 客户端时设为 `1`，关闭本地端口探测兜底（Docker/原生均建议开启） |
| `TZ` | `Asia/Shanghai` | 时区（影响签到/调度的时间判定） |

## 🐧 Debian 原生部署（systemd）

适用：Debian 12 bookworm（glibc 2.36），不安装 Docker，单二进制由 systemd 托管。运行期不需要 Node.js、Python、数据库服务或 OpenSSL。

### 1. 获取发布包

二选一：

- **GitHub Actions**：仓库 Actions → `Build Debian Native Package` → 下载 `aiwork-server-linux-amd64` 产物；
- **本地构建**（需 Debian 12 + Node.js 20 + Rust 1.89+）：

```bash
bash scripts/build_debian.sh
# 产物：release/aiwork-server_<版本>_linux_amd64.tar.gz
```

> 构建环境必须为 Debian 12 或 glibc 不高于 2.36；在 Ubuntu 24.04 等更高 glibc 环境构建的二进制无法在 Debian 12 运行。

### 2. 安装

```bash
tar xzf aiwork-server_<版本>_linux_amd64.tar.gz
cd aiwork-server_<版本>_linux_amd64
sudo bash deploy/install.sh
```

安装脚本（幂等，可重复执行用于升级）：

1. 创建系统用户 `aiwork`；
2. 安装二进制与 `dist/` 到 `/opt/aiwork`；
3. 创建数据目录 `/var/lib/aiwork`（0700，归属 `aiwork`）；
4. 首次安装生成 `/etc/aiwork/aiwork.env`（0600，已存在则保留）；
5. 安装并启动 `aiwork-server.service`。

```bash
systemctl status aiwork-server
journalctl -u aiwork-server -f
sudo cat /var/lib/aiwork/conf/admin_token
```

浏览器访问 `http://127.0.0.1:8080`（服务器本机），或经反向代理后的域名。

> 默认只监听 `127.0.0.1:8080`。公网访问请通过 Caddy/Nginx 反代并启用 TLS（见下文反代章节）。

### 3. 配置

编辑 `/etc/aiwork/aiwork.env` 后执行 `sudo systemctl restart aiwork-server`。原生部署关键项：

```bash
AIWORK_DATA_DIR=/var/lib/aiwork
AIWORK_WEB_DIST=/opt/aiwork/dist
AIWORK_LISTEN_ADDR=127.0.0.1:8080
AIWORK_DISABLE_LOCAL_QUOTA=1
TZ=Asia/Shanghai
```

- `AIWORK_ADMIN_TOKEN` 留空 → 首启生成 `conf/admin_token`；
- `AIWORK_VAULT_KEY` 留空 → 首启生成 `conf/vault_key.bin`；
- `AIWORK_DISABLE_LOCAL_QUOTA=1` 必须保留，避免云端取数失败时串行探测本机端口；
- 若修改 `AIWORK_DATA_DIR`，必须同步调整 systemd 单元的 `StateDirectory` 与 `ReadWritePaths`（默认 `/var/lib/aiwork`）。

### 4. 升级

```bash
sudo systemctl stop aiwork-server
sudo install -m 0755 aiwork-server /opt/aiwork/aiwork-server
sudo rm -rf /opt/aiwork/dist
sudo install -d -m 0755 /opt/aiwork/dist
sudo cp -R dist/. /opt/aiwork/dist/
sudo systemctl start aiwork-server
```

数据全部在 `/var/lib/aiwork`，升级不丢；SQLite 迁移幂等，首启自动执行。

### 5. 备份与日志

```bash
# 备份（停机一致性最好）
sudo tar czf aiwork-backup-$(date +%F).tgz -C /var/lib aiwork

# 运行日志
journalctl -u aiwork-server -f
ls /var/lib/aiwork/logs
```

**必须备份**：SQLite 库、`/var/lib/aiwork/conf/vault_key.bin`、`/var/lib/aiwork/conf/admin_token`。`vault_key.bin` 丢失 = 全部账号凭据永久不可解密。

### 6. 与 Docker 部署的差异

| 项 | Docker | Debian 原生 |
|---|---|---|
| 进程 | 容器内 PID 1 | systemd 托管 |
| 数据目录 | `./data:/app/data` | `/var/lib/aiwork` |
| 环境变量 | compose / `.env` | `/etc/aiwork/aiwork.env` |
| 升级 | `docker compose up -d --build` | 替换二进制与 dist 后重启 |
| 监听 | 默认 `0.0.0.0:8080` | 默认 `127.0.0.1:8080` |

## 💾 数据持久化（Docker volume `./data:/app/data`）

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

## 🩺 常见问题

| 现象 | 处置 |
|---|---|
| 容器 restart 循环 | `docker compose logs aiwork-server` 看启动错误；多为 `AIWORK_VAULT_KEY` 与已有 `vault_key.bin` 不一致 |
| 登录提示 token 无效 | 令牌取自 `conf/admin_token` 文件或 env 注入值；env 变更后需 `docker compose up -d` 重建；附加管理员令牌（设置页签发）同样可登录，吊销后立即失效 |
| 签到时间不对 | 确认 `TZ=Asia/Shanghai` 未被覆盖 |
| 手动签到无进度 | 实时通道走 WebSocket `/api/ws`（连接失败前端自动回退 SSE `/api/events/checkin`）；Nginx 需带 Upgrade 升级头并对 SSE `proxy_buffering off`（见上节示例） |
| 定时任务没跑 | 任务由服务端内置调度器托管（05:30 Trae JWT 续期 / 05:40 模型列表同步 / 09:00 Trae 签到 / 09:10 WB 签到 / 10:30 WB 续期 / 23:30、23:40 积分快照），重启后自动补跑当天错过的任务；确认环境配置页对应任务开关未停用 |
