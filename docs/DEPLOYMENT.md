# Lux 部署与升级

本文档覆盖本地 Docker、HTTPS 反向代理和 Tailscale 接入。Lux 本身只提供 HTTP 服务，不负责 TLS 终止、代理缓冲或 Tailscale 身份策略。

## Docker Compose

生产环境需要分别持久化 `/config` 和 `/media`。`/config` 存放数据库选择文件、内置 SQLite（如使用）、插件、服务配置和按 UTC 日期滚动的 `logs/lux.YYYY-MM-DD.log`；`/media` 存放媒体及需要回写的 NFO/图片。媒体挂载必须读写，因为 NFO 和图片写回需要写权限：

```bash
mkdir -p config media
docker compose pull
docker compose up -d
```

上面的默认命令只启动 Lux，使用内置 SQLite。若希望由同一个 Compose 项目额外运行 PostgreSQL，先设置
强密码，再启用 `postgres` profile：

```bash
export LUX_POSTGRES_PASSWORD='change-this-before-use'
docker compose --profile postgres pull
docker compose --profile postgres up -d
```

该 PostgreSQL 服务是独立容器，不是 Lux 容器内的子进程；它的数据直接保存在项目目录
`./postgres-data`，首次启动时 Docker 会自动创建该目录。执行 `docker compose down` 或
`docker compose down -v` 都不会删除这个目录；只有手动删除 `./postgres-data` 才会删除数据库数据，
因此删除前应先备份。启用 profile 后，在 Lux 引导中选择 PostgreSQL，主机填写 Compose 服务名
`postgres`，端口填写 `5432`。也可以不启用 profile，连接外部已有的 PostgreSQL 服务。

镜像和 Compose 都以 `root`（UID 0）运行 Lux。入口脚本会创建 `/config/plugins`，并将镜像内置的 TMDb 插件包复制到持久化配置目录；不会递归修改 `/config` 或 `/media` 的所有权，因此 bind mount 到 NAS 的目录无需预先调整 UID/GID，也不会因媒体库大小增加启动遍历时间。项目自带的 TMDb 插件会在发现后自动标记为已安装、已启用。

首次部署只在内网访问 `http://127.0.0.1:8097/` 完成初始化。初始化完成后再开放反向代理入口；不要把未初始化的 setup 页面直接暴露到公网。

### 选择数据库

首次进入引导、创建第一个管理员之前，Lux 会让你选择数据库：

- `SQLite`：默认的内置数据库，不需要额外容器；数据文件是 `/config/lux.db`。
- `PostgreSQL`：连接已经在 Lux 之外运行的 PostgreSQL 服务。可以使用本 Compose 文件的可选
  `postgres` profile，也可以填写部署环境中已有的 PostgreSQL；无论哪种方式，PostgreSQL 都不在 Lux
  容器内部运行。

PostgreSQL 需要在引导前准备好数据库、用户和网络访问权限，然后在页面填写主机、端口、数据库名、用户名、密码和 SSL 模式并测试连接。选择成功后需要重启 Lux，重启时会在 PostgreSQL 空库上运行 schema migration，再继续管理员初始化。数据库密码只保存在受 `/config` 权限保护的 `/config/database.json` 中，不会返回 API、写入日志或审计事件；请将整个 `/config` 按敏感配置进行保护。

数据库后端只能在首次初始化前选择，已初始化实例不支持在线切换，也不会自动把已有 SQLite 数据迁移到 PostgreSQL。已有 `/config/lux.db` 的旧版 SQLite 实例会继续使用 SQLite，不会显示选择页面。使用 PostgreSQL 时，PostgreSQL 数据库需要单独纳入备份、恢复、容量和升级计划；SQLite 则随 `/config` 一起备份。

管理员可以在“任务与日志 → 系统日志”选择 UTC 起止日期并直接下载日志；选择单日会下载原始 `.log` 文件，跨日会下载 ZIP。也可以在宿主机使用
`docker compose logs --no-color --timestamps --since 1h lux` 查看容器 stdout。日志文件和导出内容可能包含媒体相对路径及请求诊断信息，不要公开发布；不要把 `/config` 整目录、Cookie、配置凭据或数据库文件作为日志附件发送。

### Docker Hub 镜像

`.github/workflows/dockerhub.yml` 在 Pull Request 中只构建验证，在 `main` 推送或 `v*.*.*` 标签推送时分别使用 GitHub 原生 amd64 与 ARM64 runner 构建，再合并 Docker Hub manifest；不使用 QEMU。需要在 GitHub Actions Secrets 中配置 `DOCKERHUB_USERNAME` 和 Docker Hub Access Token `DOCKERHUB_TOKEN`；镜像地址为 `docker.io/<DOCKERHUB_USERNAME>/lux`。

建议显式设置：

```dotenv
LUX_PROXY_URL=http://192.168.1.2:7890
LUX_TRUSTED_PROXY_CIDRS=127.0.0.1/32,::1/128
LUX_TMDB_READ_ACCESS_TOKEN=***
```

`LUX_PROXY_URL` 可选，用于 Lux 的出站网络请求，例如元数据、图片和人物头像下载。支持 `http://`、`https://`、`socks4://`、`socks4a://`、`socks5://` 和 `socks5h://` 代理地址；代理 URL 可包含用户名认证信息。留空时使用标准系统代理环境变量（`HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 和 `NO_PROXY`）；这些变量也可使用小写形式。代理地址不参与入站反向代理，也不改变 `.strm` 直交行为。包含认证信息的代理 URL 应只通过受保护的环境变量或 secrets 注入，不能写入日志。

IP 归属地解析使用内置的 Hiofd 协议字段，不需要额外配置；字段不会写入日志、数据库或 API。Hiofd 不可用时管理员仪表盘仍显示客户端 IP，但归属地为空。

`LUX_TRUSTED_PROXY_CIDRS` 只能填写实际代理节点或网关的地址范围，不能使用 `0.0.0.0/0`。Lux 只信任来自这些地址的 `X-Forwarded-For`。

## Nginx 反向代理

以下配置要点适用于把 `lux.example.internal` 转发到本机 8097 端口的 HTTPS 代理：

```nginx
server {
    listen 443 ssl;
    server_name lux.example.internal;

    ssl_certificate     /etc/letsencrypt/live/lux.example.internal/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/lux.example.internal/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8097;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        proxy_set_header Range $http_range;
        proxy_set_header If-Range $http_if_range;
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 1h;
        proxy_send_timeout 1h;
    }
}
```

代理应保留 `206`、`Content-Range`、`Content-Length`、`Accept-Ranges` 和 `ETag`。不能把媒体流改成缓存整文件的代理模式。`LUX_TRUSTED_PROXY_CIDRS` 应设置为 Nginx 到 Lux 之间实际可见的 peer 地址。

## Tailscale

Tailscale Serve 或同类 HTTPS 转发应只把本机 Lux HTTP listener 映射到 tailnet 内部，不要使用公开 Funnel，除非额外配置身份访问策略。转发目标为：

```text
http://127.0.0.1:8097
```

启用后从另一台 tailnet 设备验证：

1. HTTPS 证书和浏览器安全锁正常。
2. 初始化完成后登录、退出和 CSRF 请求正常。
3. MP4 `GET` 返回 `206`，Range、`Content-Range` 和 `Content-Length` 保留。
4. WebSocket/长播放请求不被短超时切断（当前 Lux 首版播放为 HTTP 流，不提供转码）。
5. 代理 peer 地址与 `LUX_TRUSTED_PROXY_CIDRS` 一致，伪造的 forwarded header 不被非可信来源接受。

## 升级与回滚边界

发布镜像使用不可变版本标签和 digest，不使用 `latest` 作为唯一发布标识：

```bash
docker build --build-arg LUX_VERSION=0.1.3 -t lux:0.1.3 .
docker compose up -d
```

启动时会自动执行当前已选择数据库的 migrations；升级前应停止写入并同时保留 `/config` 与 `/media` 的宿主机目录。当前版本不提供应用内备份/恢复或跨数据库迁移工具，也不提供 SQLite 与 PostgreSQL 之间的数据迁移；正式 NAS 发布前必须由运维侧完成配置目录、媒体目录和（如使用）PostgreSQL 数据库的快照与恢复演练。

升级后的验收最少包括：

```bash
curl --fail http://127.0.0.1:8097/health/live
curl --fail http://127.0.0.1:8097/health/ready
docker compose ps
```

随后用真实客户端执行登录、媒体库列表、详情和一次 Range 播放。真实代理、NAS 7 天运行和发布签名仍需在目标环境单独记录，不能用本机 ARM64 结果替代。

## 本机故障注入

可以在本机 ARM64 Docker 环境用受限 tmpfs 演练 SQLite 写失败和恢复；脚本会创建临时管理员、填满 `/config`，验证 ready/管理员健康/新媒体库写入错误，再删除填充文件验证恢复：

```bash
LUX_IMAGE=lux:arm64-local ./scripts/disk-write-fault-smoke.sh
```

该脚本只证明容器内 ENOSPC 的诊断和恢复契约，不替代飞牛 NAS 真实持久卷故障演练。

也可以演练媒体目录暂时不可访问以及恢复后的重新探测：

```bash
LUX_IMAGE=lux:arm64-local ./scripts/mount-loss-smoke.sh
```

该脚本通过临时目录权限撤销模拟不可访问状态，证明扫描会隔离不可用 root、保留已有条目并在恢复后重新发现 root；它不替代真实 NAS 卸载、网络中断或持久卷恢复演练。

本机还可以用临时自签名证书和 Nginx 反代演练 HTTPS、trusted proxy 和 Range 响应头：

```bash
LUX_IMAGE=lux:arm64-local ./scripts/proxy-smoke.sh
```

该脚本验证可信代理转发公网地址时的远程访问控制，以及 `206`、`Content-Range`、`Content-Length`、`Accept-Ranges` 和 `ETag` 的保留；自签名证书和本机 Docker 网络不替代真实 Tailscale/HTTPS 实机验证。

扫描完成后可以用 ARM64 容器端到端验证媒体探测和 Emby 播放信息：

```bash
LUX_IMAGE=lux:arm64-local ./scripts/probe-smoke.sh
```

该脚本生成有效 MP4，验证扫描自动运行 `ffprobe`、媒体源进入 `READY`，并确认 `PlaybackInfo` 返回运行时长、媒体流和 `PROBE_COMPLETED` 事件；它不替代真实 NAS 媒体库验收。
