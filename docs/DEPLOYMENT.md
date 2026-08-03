# Lux 部署与升级

本文档覆盖本地 Docker、HTTPS 反向代理和 Tailscale 接入。Lux 本身只提供 HTTP 服务，不负责 TLS 终止、代理缓冲或 Tailscale 身份策略。

## Docker Compose

生产环境至少持久化 `/data`，并把媒体目录以读写方式挂载到 `/media`，因为 NFO 和图片写回需要写权限：

```bash
LUX_MEDIA_DIR=/srv/media docker compose up -d --build
```

首次部署只在内网访问 `http://127.0.0.1:8097/` 完成初始化。初始化完成后再开放反向代理入口；不要把未初始化的 setup 页面直接暴露到公网。

建议显式设置：

```dotenv
LUX_MEDIA_DIR=/srv/media
LUX_TRUSTED_PROXY_CIDRS=127.0.0.1/32,::1/128
LUX_TMDB_READ_ACCESS_TOKEN=***
```

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
docker build --build-arg LUX_VERSION=0.1.0 -t lux:0.1.0 .
docker compose up -d
```

启动时会自动执行 SQLite migrations；升级前应停止写入并保留 `/data` 卷。当前版本不提供应用内备份/恢复或跨数据库迁移工具，正式 NAS 发布前必须由运维侧完成卷快照和恢复演练。

升级后的验收最少包括：

```bash
curl --fail http://127.0.0.1:8097/health/live
curl --fail http://127.0.0.1:8097/health/ready
docker compose ps
```

随后用真实客户端执行登录、媒体库列表、详情和一次 Range 播放。真实代理、NAS 7 天运行和发布签名仍需在目标环境单独记录，不能用本机 ARM64 结果替代。

## 本机故障注入

可以在本机 ARM64 Docker 环境用受限 tmpfs 演练 SQLite 写失败和恢复；脚本会创建临时管理员、填满 `/data`，验证 ready/管理员健康/新媒体库写入错误，再删除填充文件验证恢复：

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
