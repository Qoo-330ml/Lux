# Lux

![Lux logo](logo.svg)

Lux 是面向 NAS 的个人媒体服务端：用 Rust 提供高效、可诊断的媒体索引与播放服务，用 React + TypeScript 提供同源 Web 客户端（暂不支持web播放），并通过 Emby 兼容 API 连接 VidHub、SenPlayer 等第三方客户端。

Lux的出现是为了解决Emby在面临大库时遇到的内存占用过大、加载慢等痛点。虽然Emby的第三方客户端开发环境很好，大批量的第三方播放器涌现弥补了Emby的一些问题，但指标不治本，作为一个媒体管理者，打开emby网页对于我来说真的变成了一件很痛苦的事情。于是本项目因运而生，Lux兼容了Emby的大部分接口，可以直接使用第三方emby客户端进行连接Lux，获得很好的播放体验，同时媒体库的占用也大大降低，流畅性得到了提高。

当然，还要说在前面的是，Lux现在不支持web播放，本身web播放，浏览器解码能力不行，体验只会差，可以配合比如harbor、vidhub、hills等优秀第三方emby客户端来连接Lux。

## 特性

- 电影、电视剧和混合媒体库，支持一个媒体库配置多个根路径。
- 基于文件事件的增量扫描，以及可暂停、恢复、取消和重试的后台任务。
- 本地 NFO 与图片优先，支持候选匹配、元数据锁定、图片管理和原子写回。
- 本地媒体直放、单区间 Range、外挂字幕和多媒体源/多版本选择。
- `.strm` 外部播放地址支持；播放路径不会主动探测或代理远程媒体。
- Lux Web：登录、首页、媒体库浏览、搜索、筛选、详情、继续观看、收藏、账户和管理控制台。
- Emby 兼容层：连接、认证、媒体库浏览、搜索、播放、进度、收藏、字幕和弹幕相关接口持续完善。
- 用户、会话、媒体库 ACL、下载权限、审计事件和结构化日志。
- 可选 SQLite 或外部 PostgreSQL；默认使用 `/config/lux.db` 中的 SQLite。
- 独立进程插件运行时，支持 TMDb、媒体探测和 IP 归属地等插件能力。

## 快速开始

### Docker Compose（推荐）

需要 Docker Engine 和 Docker Compose v2：

```bash
services:
  lux:
    image: pdzhou/lux:latest
    container_name: lux
    ports:
      - "8097:8097"
    volumes:
      - ./config:/config:rw
      - ./media:/media:rw
    restart: unless-stopped
```

打开 <http://localhost:8097/>，按引导完成数据库选择和第一个管理员创建。

默认挂载关系如下：

| 宿主机 | 容器 | 用途 |
|---|---|---|
| `./config` | `/config` | 数据库、插件、配置和日志；必须持久化 |
| `./media` | `/media` | 媒体库；需要读写权限以支持 NFO 和图片写回 |

如果媒体实际位于 NAS 的其他目录，请先编辑 [`compose.yaml`](compose.yaml) 中的 `/media` 挂载，再在 Lux 管理界面把容器内路径（例如 `/media/Movies`）添加为媒体库根路径。

首次初始化建议只在局域网完成。初始化完成后，再通过 HTTPS 反向代理或 Tailscale 对外提供访问；不要把未初始化的 setup 页面直接暴露到公网。

### 使用 PostgreSQL（可选）

PostgreSQL 不是 Lux 容器内的子进程。可以使用 Compose 的 `postgres` profile，也可以填写部署环境中已有的 PostgreSQL：

```bash
services:
  lux:
    image: pdzhou/lux:latest
    container_name: lux
    user: "0:0"
    ports:
      - "8097:8097"
    volumes:
      - ./config:/config:rw
      - ./media:/media:rw
    restart: unless-stopped

  postgres:
    image: postgres:16-alpine
    container_name: lux-postgres
    environment:
      POSTGRES_DB: ${LUX_POSTGRES_DB:-lux}
      POSTGRES_USER: ${LUX_POSTGRES_USER:-lux}
      POSTGRES_PASSWORD: ${LUX_POSTGRES_PASSWORD:-}  #-后面填写密码
    volumes:
      # Store PostgreSQL data directly in the project directory.
      - ./postgres-data:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U $${POSTGRES_USER} -d $${POSTGRES_DB}"]
      interval: 10s
      timeout: 5s
      retries: 5
    restart: unless-stopped
```

在首次引导中选择 PostgreSQL 时，若使用本 Compose 提供的数据库，主机填写 `postgres`，端口填写 `5432`。数据库后端只能在创建第一个管理员之前选择；当前不支持已初始化实例在线切换，也不会自动执行 SQLite 到 PostgreSQL 的迁移。


## 第一次使用

1. 打开 Web 首页，选择 SQLite 或 PostgreSQL。
2. 创建第一个管理员账号。
3. 在管理控制台创建媒体库，选择电影、电视剧或混合类型。
4. 添加容器内可访问的根路径，例如 `/media/Movies`。
5. 启动扫描，等待媒体进入索引。
6. 按需安装并配置刮削器，然后处理低置信度的待匹配条目。

Lux 首版以直放为主，不提供音视频转码、HLS 转码或字幕格式转换。客户端是否能播放某种编码和渲染某种字幕，取决于客户端本身。


## 当前兼容性

以下是仓库中已有实测记录的摘要，不代表所有客户端或媒体编码都兼容：

| 客户端 | 实测版本 | 当前记录 |
|---|---|---|
| VidHub | 2.1.8（macOS arm64） | 添加、登录、浏览、详情、本地直放、播放状态和收藏已验证 |
| SenPlayer | 6.0.6（macOS arm64） | 添加、登录、首页、电影列表和 `.strm` 直连播放已验证；其余能力持续补测 |
| Harbor | 1.4.6（macOS arm64） | 添加、登录、进入媒体库并显示条目已验证 |
| Lux Web | Chrome 150 smoke | 登录、筛选、详情、MP4 直放、收藏、账户、管理流程和多 viewport smoke 已验证 |
| 网易爆米花 | 2.15.3（ios26） | 添加、登录、浏览、详情、本地直放、播放状态 |

如果遇到有问题的第三方emby播放器，欢迎提issue。

## 已知限制

- 首版只做直接播放，不做转码、HLS、容器转换或在线字幕下载。
- 不实现完整 Emby API；只持续覆盖目标客户端实际需要的兼容接口。
- `.strm` 播放地址会交给有权限的客户端；Lux 播放路径不替客户端隐藏其中可能存在的 token。
- 当前 Docker 镜像和 Compose 以 root 运行 Lux，以兼容不同 NAS bind mount 的 UID/GID；请保护 `/config`，不要把管理入口未经反向代理安全措施直接暴露到公网。
- 首版不包含音乐库、照片库、直播电视、DVR、DLNA、内置备份恢复或多节点高可用。


## License

Lux 的 Rust package metadata 使用 MIT 许可证。仓库中的字体资源另有对应的 SIL Open Font License 1.1 许可说明，见 [`assets/fonts/SmileySans-LICENSE.txt`](assets/fonts/SmileySans-LICENSE.txt)。
