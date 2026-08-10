# Lux

![Lux logo](logo.svg)

Lux 是面向 NAS 的个人媒体服务端：用 Rust 提供高效、可诊断的媒体索引与播放服务，用 React + TypeScript 提供同源 Web 客户端，并通过 Emby 兼容 API 连接 VidHub、SenPlayer 等第三方客户端。

Lux 目前处于 `0.1.x` 快速迭代阶段。功能边界、任务进度和验收标准以 [`docs/LUX-DEVELOPMENT.md`](docs/LUX-DEVELOPMENT.md) 为准；第三方客户端的实测结果以 [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md) 为准。

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
git clone https://github.com/Qoo-330ml/Lux.git
cd Lux

mkdir -p config media
docker compose pull
docker compose up -d
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
export LUX_POSTGRES_PASSWORD='请替换为强密码'
docker compose --profile postgres up -d
```

在首次引导中选择 PostgreSQL 时，若使用本 Compose 提供的数据库，主机填写 `postgres`，端口填写 `5432`。数据库后端只能在创建第一个管理员之前选择；当前不支持已初始化实例在线切换，也不会自动执行 SQLite 到 PostgreSQL 的迁移。

更多生产部署、HTTPS 反向代理、Tailscale、升级和备份建议见 [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md)。

## 第一次使用

1. 打开 Web 首页，选择 SQLite 或 PostgreSQL。
2. 创建第一个管理员账号。
3. 在管理控制台创建媒体库，选择电影、电视剧或混合类型。
4. 添加容器内可访问的根路径，例如 `/media/Movies`。
5. 启动扫描，等待媒体进入索引。
6. 按需安装并配置刮削器，然后处理低置信度的待匹配条目。

Lux 首版以直放为主，不提供音视频转码、HLS 转码或字幕格式转换。客户端是否能播放某种编码和渲染某种字幕，取决于客户端本身。

## 本地开发

### 环境要求

- Rust stable；仓库通过 [`rust-toolchain.toml`](rust-toolchain.toml) 固定工具链频道。
- Node.js 与 pnpm；Web 构建使用 pnpm 11。
- FFmpeg/`ffprobe`；本地媒体信息探测需要它。
- Docker；仅在需要验证镜像或 Compose 时使用。

安装 Web 依赖：

```bash
corepack enable
corepack prepare pnpm@11.9.0 --activate
pnpm --dir web install --frozen-lockfile
```

启动 Rust 服务和 Vite 开发服务器：

```bash
# 终端一：默认监听 127.0.0.1:8097
cargo run --bin luxd

# 终端二：Vite 监听 127.0.0.1:5173，并把 API 代理到 Rust 服务
pnpm --dir web dev
```

开发时访问 <http://127.0.0.1:5173/>。若希望由 Rust 服务直接提供 Web 静态资源，先运行 `pnpm --dir web build`，再访问 <http://127.0.0.1:8097/>。

常用环境变量如下；基础配置可从 [`.env.example`](.env.example) 开始：

| 变量 | 默认值 | 说明 |
|---|---|---|
| `LUX_HTTP_ADDR` | `127.0.0.1:8097` | HTTP 监听地址；容器中使用 `0.0.0.0:8097` |
| `LUX_CONFIG_DIR` | `./config` | 配置、SQLite 和日志目录 |
| `LUX_WEB_DIR` | 自动选择 | 指定已经构建好的 Web 静态资源目录 |
| `LUX_PROXY_URL` | 空 | Lux 发起外部请求时使用的代理 |
| `RUST_LOG` | `luxd=info,tower_http=info` | 日志过滤规则 |
| `TZ` | 系统/镜像默认值 | 日志和界面使用的时区 |

## 检查与构建

Rust 检查：

```bash
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

Web 检查：

```bash
pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
```

完整检查：

```bash
./scripts/check-all.sh
```

本地 ARM 验证请记录 `uname -m`。开发机上的 `arm64`/`aarch64-apple-darwin` 结果不能替代目标 x86_64 NAS 的性能结论。

## 发布 Docker 镜像（维护者）

`.github/workflows/dockerhub.yml` 在 Pull Request 中只构建验证；推送到 `main` 或推送 `v*.*.*` 标签时，使用原生 amd64/ARM64 runner 构建并发布 `linux/amd64`、`linux/arm64` manifest。

在 GitHub 仓库的 **Settings → Secrets and variables → Actions** 中配置：

- `DOCKERHUB_USERNAME`：Docker Hub 用户名。
- `DOCKERHUB_TOKEN`：Docker Hub Access Token，不要使用账户密码。

镜像名为 `docker.io/<DOCKERHUB_USERNAME>/lux`。发布版本示例：

```bash
git tag v0.1.8
git push origin v0.1.8
```

## 架构概览

Lux 首版是一个模块化单体：一个 Rust 进程、一个数据库、一个 Web 静态资源目录，以及多个有界的后台 worker。

```text
VidHub / SenPlayer / Infuse / Lux Web
                  │
        Axum HTTP + Emby 兼容层
                  │
       application services / domain
                  │
       storage（SQLite / PostgreSQL）
                  │
     扫描、元数据、播放、插件后台任务
```

主要目录：

| 目录 | 职责 |
|---|---|
| `src/api/` | Lux API、Emby 兼容 API、认证、播放和 DTO 映射 |
| `src/application/` | 扫描、目录、元数据、插件、播放、任务等应用服务 |
| `src/domain/` | 领域 ID、时间和核心类型 |
| `src/storage/` | SQL、迁移和数据库访问 |
| `src/auth/` | 用户、密码、会话和 Emby 认证 |
| `src/observability/` | 结构化日志、资源和健康信息 |
| `web/` | React + TypeScript Web 客户端 |
| `plugins/` | 插件 manifest 与插件包相关资源 |
| `tests/` | Rust 集成、协议、播放和管理测试 |
| `docs/` | 产品规格、部署、兼容性、性能和 ADR |

Emby 路由、DTO 和认证兼容逻辑与 Lux 自有 API 保持边界，不直接污染内部领域模型。扫描、NFO 解析、媒体探测和在线刮削都在后台执行，不进入登录、浏览、搜索和播放请求路径。

## 当前兼容性

以下是仓库中已有实测记录的摘要，不代表所有客户端或媒体编码都兼容：

| 客户端 | 实测版本 | 当前记录 |
|---|---|---|
| VidHub | 2.1.8（macOS arm64） | 添加、登录、浏览、详情、本地直放、播放状态和收藏已验证 |
| SenPlayer | 6.0.6（macOS arm64） | 添加、登录、首页、电影列表和 `.strm` 直连播放已验证；其余能力持续补测 |
| Harbor | 1.4.6（macOS arm64） | 添加、登录、进入媒体库并显示条目已验证 |
| Lux Web | Chrome 150 smoke | 登录、筛选、详情、MP4 直放、收藏、账户、管理流程和多 viewport smoke 已验证 |
| Infuse | 未测试 | 尚未形成可发布的兼容性结论 |

完整请求序列、版本、证据和已知差异见 [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md)。

## 已知限制

- 首版只做直接播放，不做转码、HLS、容器转换或在线字幕下载。
- 不实现完整 Emby API；只持续覆盖目标客户端实际需要的兼容接口。
- `.strm` 播放地址会交给有权限的客户端；Lux 播放路径不替客户端隐藏其中可能存在的 token。
- 当前 Docker 镜像和 Compose 以 root 运行 Lux，以兼容不同 NAS bind mount 的 UID/GID；请保护 `/config`，不要把管理入口未经反向代理安全措施直接暴露到公网。
- 首版不包含音乐库、照片库、直播电视、DVR、DLNA、内置备份恢复或多节点高可用。

## 文档

- [`docs/LUX-DEVELOPMENT.md`](docs/LUX-DEVELOPMENT.md)：产品规格、架构边界、任务清单和验收标准，项目事实来源。
- [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md)：Docker、反向代理、Tailscale、升级、备份和故障排查。
- [`docs/API.md`](docs/API.md)：Lux API 与 Emby 兼容 API 说明。
- [`docs/PLUGIN-SDK.md`](docs/PLUGIN-SDK.md)：独立插件包、manifest 和运行时协议。
- [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md)：第三方客户端兼容性矩阵与实测证据。
- [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md)：性能基准和资源记录。
- [`docs/decisions/`](docs/decisions/)：架构决策记录（ADR）。

## 参与开发

开始前请阅读 [`AGENTS.md`](AGENTS.md) 和 [`docs/LUX-DEVELOPMENT.md`](docs/LUX-DEVELOPMENT.md)。开发遵循以下约束：

- 一次只处理一个 `LUX-*` 任务，不提前实现后续阶段。
- 行为变化采用测试驱动开发，保持小步、可编译、可独立回滚。
- SQL 留在 `storage`，HTTP handler 只负责协议解析、校验、服务调用和 DTO 映射。
- 不提交密码、token、Cookie、真实 `.strm` URL 或用户数据。
- 提交 PR 前运行与改动范围相关的检查，并在描述中说明改动文件、验收结果、测试结果和剩余风险。

欢迎提交 issue 和 pull request；如果是客户端兼容性问题，请同时提供客户端版本、平台、脱敏后的请求路径、状态码和关键响应字段。

## License

Lux 的 Rust package metadata 使用 MIT 许可证。仓库中的字体资源另有对应的 SIL Open Font License 1.1 许可说明，见 [`assets/fonts/SmileySans-LICENSE.txt`](assets/fonts/SmileySans-LICENSE.txt)。
