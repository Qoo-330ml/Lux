# Lux 部署与升级

本文档覆盖本地 Docker、HTTPS 反向代理和 Tailscale 接入。Lux 本身只提供 HTTP 服务，不负责 TLS 终止、代理缓冲或 Tailscale 身份策略。

## Docker Compose

生产环境需要分别持久化 `/config` 和 `/media`。`/config` 存放数据库选择文件、内置 SQLite（如使用）、插件、服务配置和按 UTC 日期滚动的 `logs/lux.YYYY-MM-DD.log`；启用“按显示尺寸提供图片”后，派生的图片变体存放在 `/config/image-cache/v1/`，属于可删除、可重建的运行缓存，不需要纳入备份，也不会改写 `/media` 中的原图；`/media` 存放媒体及需要回写的 NFO/图片。媒体挂载必须读写，因为 NFO 和图片写回需要写权限：

```bash
mkdir -p config media
docker compose pull
docker compose up -d
```

局域网发现使用 UDP `7359`，Compose 已发布该端口。默认响应地址按收到请求的网络接口和 HTTP 端口生成；
如果 Lux 位于 Docker、反向代理后面或有多张网卡，建议在 `.env` 中设置客户端实际可访问的基地址：

```dotenv
LUX_DISCOVERY_ADVERTISE_URL=https://lux.example.internal
```

该值只接受 HTTP/HTTPS，不得包含用户名、密码、查询参数或 fragment。`LUX_DISCOVERY_BIND_ADDR`
可用于受限网络部署，默认是 `0.0.0.0:7359`；它只影响 UDP 监听，不改变 HTTP 监听地址。发现响应不携带
认证信息，Prism 仍须验证响应地址，并额外探测 UDP 来源地址对应的 HTTP 端点。

Compose 默认把 Lux 容器的内存硬上限设为 `2g`。这不是正常内存预算；Lux 的默认扫描常驻内存目标仍是
750 MB 以下。有限的 cgroup 上限让 Lux 能读取真实的容器内存压力：使用率达到 70% 时后台 worker
并发减半，达到 85% 时降为单 worker；若进程仍越过硬上限，Docker 会终止并按重启策略拉起容器，
避免耗尽 NAS 宿主机内存。可按部署容量在 `.env` 中覆盖，但不建议取消上限：

```dotenv
LUX_MEMORY_LIMIT=2g
LUX_MALLOC_ARENA_MAX=2
```

`LUX_MALLOC_ARENA_MAX` 限制 Debian/glibc 为多线程分配的堆 arena 数，减少任务结束后每线程 arena
保留大量匿名页的概率；镜像默认值同样为 `2`。该设置可覆盖，但应先在相同 NAS 架构和媒体库上比较
任务耗时、RSS 峰值和任务结束后的 RSS，再提高数值。修改后需重建容器，单纯重启旧容器不会应用新的
Compose 配置：

```bash
docker compose up -d --force-recreate lux
docker inspect --format '{{.HostConfig.Memory}}' lux
docker exec lux sh -c 'cat /sys/fs/cgroup/memory.max; printf "%s\n" "$MALLOC_ARENA_MAX"'
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

镜像和 Compose 都以 `root`（UID 0）运行 Lux。入口脚本只创建 `/config/plugins`，不会向其中复制插件；插件包由管理员从配置的插件商店下载、校验并显式安装。入口脚本不会递归修改 `/config` 或 `/media` 的所有权，因此 bind mount 到 NAS 的目录无需预先调整 UID/GID，也不会因媒体库大小增加启动遍历时间。

首次部署只在内网访问 `http://127.0.0.1:8097/` 完成初始化。初始化完成后再开放反向代理入口；不要把未初始化的 setup 页面直接暴露到公网。

### 选择数据库

首次进入引导、创建第一个管理员之前，Lux 会让你选择数据库：

- `SQLite`：默认的内置数据库，不需要额外容器；数据文件是 `/config/lux.db`。
- `PostgreSQL`：连接已经在 Lux 之外运行的 PostgreSQL 服务。可以使用本 Compose 文件的可选
  `postgres` profile，也可以填写部署环境中已有的 PostgreSQL；无论哪种方式，PostgreSQL 都不在 Lux
  容器内部运行。

PostgreSQL 需要在引导前准备好数据库、用户和网络访问权限，然后在页面填写主机、端口、数据库名、用户名、密码和 SSL 模式并测试连接。选择成功后，点击页面上的“重启 Lux”按钮；Lux 会在同一容器内优雅重启服务进程，在 PostgreSQL 空库上运行 schema migration，再继续管理员初始化。数据库密码只保存在受 `/config` 权限保护的 `/config/database.json` 中，不会返回 API、写入日志或审计事件；请将整个 `/config` 按敏感配置进行保护。

数据库后端只能在首次初始化前选择，已初始化实例不支持在线切换，也不会自动把已有 SQLite 数据迁移到 PostgreSQL。已有 `/config/lux.db` 的旧版 SQLite 实例会继续使用 SQLite，不会显示选择页面。使用 PostgreSQL 时，PostgreSQL 数据库需要单独纳入备份、恢复、容量和升级计划；SQLite 则随 `/config` 一起备份。

管理员可以在“任务与日志 → 系统日志”选择 UTC 起止日期并直接下载日志；选择单日会下载原始 `.log` 文件，跨日会下载 ZIP。也可以在宿主机使用
`docker compose logs --no-color --timestamps --since 1h lux` 查看容器 stdout。日志文件和导出内容可能包含媒体相对路径及请求诊断信息，不要公开发布；不要把 `/config` 整目录、Cookie、配置凭据或数据库文件作为日志附件发送。

第三方 Emby 客户端（Infuse、Emby 官方、Hills、Filmly、Fileball 等）是否发送尺寸参数、图片是否命中变体缓存，可以只看图片诊断这一组日志：`docker compose logs --no-color --since 1h lux 2>&1 | grep emby_image_diag`。每张成功返回的 Emby 图片一行（只有真正返回图片的 2xx/304 响应才有，鉴权失败、404/5xx 不会记录），命中缓存时是 `outcome=variant` 加 `variant_width`，回退原图时是 `outcome=original` 加 `reason`；这些字段已脱敏（不含 token、URL、查询串、Cookie、IP 和条目 ID），客户端/设备名只保留截断后的产品名。

### SQLite 写入耐久性（LUX_SQLITE_SYNCHRONOUS）

内置 SQLite 使用 WAL。每条池连接在建立时统一设置 `synchronous`，可以用部署环境变量在 `FULL` 与
`NORMAL` 之间切换：

| 选项 | 默认 | 取值 | 说明 |
|---|---|---|---|
| `LUX_SQLITE_SYNCHRONOUS` | `FULL` | `full`、`normal`（不区分大小写） | 每条新建连接执行一次 `PRAGMA synchronous = <FULL\|NORMAL>`；留空按默认处理，其他值回退 `FULL` 并在启动日志里留下一条告警 |

`FULL` 是 SQLite 自己的默认值：每次提交都把 WAL 刷到磁盘。`NORMAL` 在 WAL 模式下不会损坏数据库，
断电时最多丢掉最后几次已提交的事务，对小事务密集的路径（例如每 10 秒一次的播放进度上报）收益最大，
在机械盘 NAS 上尤其明显。**默认值保持 `FULL`**；在 NAS 上做完 A/B 并确认可以接受断电丢最后几次提交
之后，才建议把默认切换纳入决定。该变量只作用于内置 SQLite 的池连接：`Database` 只把它写进 SQLite 的
`after_connect`；PostgreSQL 后端的连接初始化不设置 `synchronous`，扫描事务的 PostgreSQL 局部
`synchronous_commit = off` 也保持不变。`:memory:` 只在测试里以裸池形式出现，不经过这条连接初始化路径。

在 NAS 上做 A/B 的方法（两轮使用同一份数据库副本和同一组操作，交替顺序再跑一轮以抵消缓存影响）：

```bash
# 1. 停机，为每轮准备一份完整的 /config 副本（含 database.json 与 lux.db）
docker compose stop lux
cp -a config config-ab-full
cp -a config config-ab-normal

# 2. 每轮用独立的 /config 副本和独立容器名，避免两次运行互相污染
docker run -d --name lux-ab \
  -e LUX_SQLITE_SYNCHRONOUS=full \
  -v "$PWD/config-ab-full:/config" -v "$PWD/media:/media" \
  -p 8097:8097 -p 7359:7359/udp pdzhou/lux:latest
# 跑完同一套操作序列（连续播放上报，混一次小库扫描）后记录墙钟、CPU、iowait、
# 播放上报延迟与日志里的写锁等待，然后停掉容器
docker stop lux-ab && docker rm lux-ab

docker run -d --name lux-ab \
  -e LUX_SQLITE_SYNCHRONOUS=normal \
  -v "$PWD/config-ab-normal:/config" -v "$PWD/media:/media" \
  -p 8097:8097 -p 7359:7359/udp pdzhou/lux:latest

# 3. 确认连接实际生效：启动日志有一条“sqlite connection durability level”记录，
#    FULL 显示 synchronous=FULL，NORMAL 显示 synchronous=NORMAL；
#    再对比两轮的 /config/lux.db-wal 增长与 checkpoint 次数
docker logs lux-ab 2>&1 | grep 'sqlite connection durability level'

# 4. 断电恢复证据：在 NORMAL 副本上强杀容器后重启，确认数据库能打开、任务可继续
docker kill --signal=KILL lux-ab && docker start lux-ab
```

Compose 部署若希望长期保留该设置，可在 `.env` 里写 `LUX_SQLITE_SYNCHRONOUS=normal`，
并给 `compose.yaml` 的 `lux.environment` 加上对应转发（默认的 Compose 文件目前没有转发这一项，
只写 `.env` 不会进入容器）；非容器部署直接作为进程环境变量设置即可。

只在一台 ARM 开发机上跑出的数字不能外推到 NAS：机械盘、低功耗 x86、容器 I/O 限速都会改变 fsync
成本。批量扫描的 A/B 数据不代表小事务，因为批量提交已经把 fsync 压到很少；要用播放上报这类小事务
路径判断收益。

### Docker Hub 镜像

`.github/workflows/dockerhub.yml` 在 Pull Request 中只构建验证；推送 `test` 分支时发布 Docker Hub `test` 标签，推送 `main` 分支时发布 `latest` 标签。两种分支都分别使用 GitHub 原生 amd64 与 ARM64 runner 构建，再合并 Docker Hub manifest；不使用 QEMU。runtime 依赖按 `RUNTIME_IMAGE_TAG` 发布到独立的 `lux-runtime` 仓库：每个架构只有在对应版本标签不存在时才构建，应用构建随后解析该架构镜像的 digest，并以 `image@digest` 引用。只有在 Debian/FFmpeg 依赖变化时才提升 `RUNTIME_IMAGE_TAG`；普通应用更新不会重新生成 runtime 层。应用镜像仍会携带 runtime 层，Docker Hub 会按 layer digest 复用它。需要在 GitHub Actions Secrets 中配置 `DOCKERHUB_USERNAME` 和 Docker Hub Access Token `DOCKERHUB_TOKEN`；应用镜像地址为 `docker.io/<DOCKERHUB_USERNAME>/lux`，runtime 镜像地址为 `docker.io/<DOCKERHUB_USERNAME>/lux-runtime`。

同步 `test` 到 `main` 时，在 GitHub Actions 选择 `main` 并手动运行 `.github/workflows/sync-test-to-main.yml`。该 workflow 需先进入默认分支才可运行；运行后会将 `test` 合并并推送到 `main`，再调用 Docker Hub workflow 发布 `latest`。如果 `test` 已包含在 `main` 中，则不创建新提交，也不重复发布。

在 GitHub Actions 手动运行 `.github/workflows/promote-dockerhub.yml`，可将 `main` 当前版本的 `latest` 多架构 manifest 复制为语义化版本标签；该操作不会覆盖 `latest`。部署时应优先使用版本标签或 digest。

建议显式设置：

```dotenv
LUX_PROXY_URL=http://192.168.1.2:7890
```

`LUX_PROXY_URL` 可选，用于 Lux 及其外置插件的出站网络请求，例如元数据、图片和人物头像下载。支持 `http://`、`https://`、`socks4://`、`socks4a://`、`socks5://` 和 `socks5h://` 代理地址；代理 URL 可包含用户名认证信息。留空时使用标准系统代理环境变量（`HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY` 和 `NO_PROXY`）；这些变量也可使用小写形式。代理地址不参与入站反向代理，也不改变 `.strm` 直交行为。包含认证信息的代理 URL 应只通过受保护的环境变量或 secrets 注入，不能写入日志。TMDb/豆瓣凭据请在各自插件的专属配置中设置，不再通过 `LUX_TMDB_*` 环境变量注入 Lux 主进程。

外置插件的请求由插件子进程自己发出，宿主只通过 `LUX_PROXY_URL` 下发代理，而且插件无法读取 `NO_PROXY` 按域名绕过。需要让某个插件不经代理直连时，在 `config/plugin_proxy_bypass` 中每行写一个插件 id（也接受逗号分隔；`#` 起注释），或用 `LUX_PLUGIN_PROXY_BYPASS` 提供同样的列表；文件优先于环境变量。名单内的插件启动时不会收到 `LUX_PROXY_URL`，并会移除继承的 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`（含小写形式）。例如某些上游按代理出口 IP 拒绝访问（Cloudflare 403），而宿主自身地址可以正常访问，就应把该章节源加入此名单，而不是放宽全局代理规则。


本地文件索引默认使用 4 路 worker，在线元数据刮削默认使用 4 路 worker；FILL_MISSING 的进程级上限也是 4。官方 Docker 镜像和 Compose 默认注入 LUX_SCAN_CONCURRENCY=4、LUX_FFMPEG_CONCURRENCY=2；LUX_PROBE_CONCURRENCY 留空时按有效 CPU 容量自动计算进程级探测预算 ceil(CPU 核数 × 4)，上限 512，运行中活动并发按本地 ffprobe 成功完成速率自适应调整。显式 LUX_PROBE_CONCURRENCY 仍覆盖自动预算。自适应调度不读取 CPU 使用率；首页延迟或 cgroup 内存压力只用于保护性降档。

LUX_BACKGROUND_CPU_BUDGET 为扫描准备、指纹、后台 NFO/图片计算、ffprobe 和缩略图 ffmpeg 提供进程级共享准入；留空时按可见 CPU 与 cgroup CPU 限额推导 max(1, min(CPU 数) - 1)，自动值上限 16，显式整数范围为 1–32，进程启动时读取。它不构成 OS CPU 配额，单个 ffprobe/ffmpeg 仍可能使用多个 CPU 线程；探测器自适应预算与后台共享预算共同限制活动工作。

LUX_SCAN_CONCURRENCY 只限制同一时刻活动的文件扫描任务/扫描工作项上限，不是 Tokio runtime 使用的 OS 线程总数；LUX_PROBE_CONCURRENCY 和 LUX_FFMPEG_CONCURRENCY 独立限制 ffprobe、ffmpeg 子进程，LUX_BACKGROUND_CPU_BUDGET 再对列出的后台 CPU 工作做共享准入。LUX_SCAN_CONCURRENCY 范围为 1–1024，LUX_PROBE_CONCURRENCY 范围为 1–512，LUX_FFMPEG_CONCURRENCY 范围为 1–4。非容器部署未设置扫描环境变量时，新建媒体库索引默认 4 路；资源自适应仍可能在 CPU、内存、前台延迟或存储延迟较高时降低实际扫描并发，SQLite 批量入库保持单写者。

LUX_DATABASE_MAINTENANCE 控制内置数据库维护，默认开启；设为 `0`、`false`、`off` 或 `no` 可完全关闭。维护在启动后延迟执行一次，之后按每 24 小时一轮在后台运行，并且只在没有扫描或元数据任务运行时才开始；检测到活动任务则跳过本轮。它只做三类低风险动作：对死元组达到判定阈值的表执行 `VACUUM (ANALYZE)`、刷新 `media_search` 的统计信息，以及对满足条件的热点大表写入表级 `autovacuum_*` 参数。它不会删除业务数据、删除索引、执行 `VACUUM FULL` 或 `REINDEX`，也不会修改全局 `postgresql.conf` 或重启数据库进程。SQLite 后端只在 `freelist` 同时达到最小页数和文件占比阈值时才处理。默认仍是整库 `VACUUM`：它会重写整个文件并在过程中独占写锁，受 `LUX_SQLITE_VACUUM_TIMEOUT_SECONDS`（默认 120 秒）约束，超大库通常超时并在日志里留下提示——这类库需要运维自行在停机窗口执行一次 `VACUUM`。设置 `LUX_SQLITE_AUTO_VACUUM=incremental` 后，新建数据库会在第一个表创建前写入 `auto_vacuum=INCREMENTAL`，维护轮改用 `PRAGMA incremental_vacuum` 分小步把空闲页归还文件系统，单步持锁很短，删除大型媒体库后无需重建整个库；代价是每个空闲页都要维护指针映射，实测删除路径约慢 1.7 倍（400 万行 38 秒 vs 64 秒），因此默认不开启。该开关只影响新建库，已有库切换仍需要一次整库 `VACUUM`。autovacuum 参数对 SQLite 不适用。LUX_DATABASE_MAINTENANCE_INTERVAL_SECONDS 调整轮次间隔（`0` 表示停用周期任务），LUX_DATABASE_MAINTENANCE_INITIAL_DELAY_SECONDS 调整启动后的首次延迟；非法值回退默认。 LUX_TASK_HISTORY_RETENTION_DAYS 控制已完成/失败/取消的扫描、元数据、章节、弹幕和 STRM 探测任务历史保留天数（默认 14，0 表示停用周期清理）；`metadata_candidates` 中已拒绝/过期的候选也会一并分批清理。 内置 PostgreSQL 通过 `shared_preload_libraries=pg_stat_statements` 和 `pg_stat_statements.track=all` 预加载查询统计；启动后 Lux 会在后台尝试一次 `CREATE EXTENSION IF NOT EXISTS pg_stat_statements`，仅当服务器已预加载该库且业务库尚未安装时才执行；这个动作与 `LUX_DATABASE_MAINTENANCE` 无关，失败只记一条警告、不影响启动。该扩展不是 PostgreSQL 的受信任扩展，需要超级用户权限：官方 Compose 的 `postgres` profile 使用的账号是超级用户，会自动成功；外部或托管 PostgreSQL 若账号权限不足，需要管理员自行在业务库执行 `CREATE EXTENSION IF NOT EXISTS pg_stat_statements;`。服务器没有预加载时 Lux 不会创建（否则每次查询都会报错），需要先在 `postgresql.conf` 或启动命令里设置 `shared_preload_libraries=pg_stat_statements` 并重启 PostgreSQL。设置 `LUX_POSTGRES_QUERY_STATISTICS_EXTENSION=0`（也接受 `false`、`off`、`no`）可关闭自动创建，默认开启。扩展就绪后，管理员数据库诊断才会返回 `queryStatistics`。若运行 PostgreSQL 的容器使用较小的 `/dev/shm`，并行索引清理可能因共享内存不足失败，此时可对该容器设置 `shm_size`。

符号链接媒体：扫描不会跟随未经许可的链接。文件链接总是解析到目标（链接名决定是否候选，目标决定大小、mtime 与身份）；目录链接由 `LUX_SYMLINK_POLICY` 决定是否进入——`none` 不进入，`root_children`（默认）只进入媒体根路径的直接子目录链接，`all` 允许任意层级的目录链接。解析后的目标必须落在所属媒体库根内，或用 `LUX_SYMLINK_TARGET_ROOTS`（冒号分隔的绝对路径）显式列出的目标根内，否则按不存在处理并记录告警。一次扫描中同一 `(设备, inode)` 目录只遍历一次，因此链接回指祖先不会形成循环，链接子树与真实子树也不会产生两份条目。删除源自链接的条目只删除链接本身，目标文件保留。需要把现有符号链接一次性转成 `.strm` 时，可使用 `python3 scripts/symlinks-to-strm.py --root /media/xxx` 先预览，确认后加 `--apply`。

修改 Compose 中的该变量后需要重新创建容器，单纯执行 `docker restart lux` 不会更新容器环境变量：

```bash
docker compose up -d --force-recreate lux
docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' lux | grep -E '^LUX_(SCAN|PROBE|FFMPEG)_CONCURRENCY=|^LUX_BACKGROUND_CPU_BUDGET='
```

如果修改了 Dockerfile 或构建上下文，`--force-recreate` 只会重新创建容器，不会重新构建镜像，必须先执行一次镜像 build。当前 `compose.yaml` 只引用已构建的 `image`，没有 `build` 配置；本地构建应使用仓库的 Bake 配置，并给镜像设置与 Compose 相同的标签：

如果使用另行配置了 `build:` 的 Compose 文件，Dockerfile 变更时必须使用
`docker compose up -d --build --force-recreate lux`；本仓库的 Compose 没有 `build:`，因此按下面的 Bake 命令执行构建。

```bash
docker buildx bake --load --set app.tags=pdzhou/lux:latest app
docker compose up -d --force-recreate lux
```

后台“转码”页的设置只作用于实时播放的 HLS 转码（是否启用、编码器、并发、磁盘与画质）。缩略图与截图、媒体信息探测、章节检测等后台任务使用独立的 FFmpeg/FFprobe 预算（`LUX_FFMPEG_CONCURRENCY` 等），不受该开关影响。播放转码的并发上限、磁盘配额、码率与画质参数默认由管理员在后台“转码”页配置，保存在数据库 `server_settings.transcoding`，保存后对新建播放会话生效，正在播放的会话保持启动时的参数。两个部署级环境变量仍然优先于后台设置，用于设备/驱动相关的紧急覆盖：

- `LUX_HLS_HW_ENCODER`：显式指定硬件编码器（`h264_nvenc`、`h264_qsv`、`h264_vaapi`、`h264_videotoolbox`）。设置后忽略后台选择的编码器；未设置时使用后台设置，后台为 `NONE` 时硬件档位不可用并降级为软件转码。
- `LUX_HLS_GLOBAL_DISK_BUDGET_BYTES`：覆盖全局 HLS 磁盘预算；未设置时使用后台的“全局磁盘预算”（0 表示不限制）。

转码临时目录位于配置目录下的 `web-playback/`，只保存当前会话的分片，会话停止、过期或服务重启后会回收。

### 服务端 GPU 转码

硬件编码默认关闭。选择编码器后，Lux 会在启动、保存设置和手动“重新检测”时用与真实转码相同的参数跑一次单帧自检：自检不通过时不会使用该编码器，播放自动走软件转码；硬件档启动失败或并发名额不足时，同一会话会自动降级为软件转码并更新会话档位，不再直接返回 502。自检只验证编码，不验证解码或 4K 实时能力。

各家 GPU 的前置条件不同：

- **Apple VideoToolbox**：仅 macOS 原生部署可用（Docker 运行在 Linux，无法使用 VideoToolbox）。
- **NVIDIA NVENC**：宿主机需安装驱动与 `nvidia-container-toolkit`，并给容器启用 GPU（`gpus: all` 或 `runtime: nvidia`）；`compose.yaml` 已提供注释示例。驱动对并发 NVENC 会话有上限，建议把“硬件转码并发上限”保持在 2–4。
- **Intel QSV / Intel/AMD VAAPI**：需要把 `/dev/dri` 映射进容器（`devices: - /dev/dri:/dev/dri`），且镜像内的 Jellyfin FFmpeg 能找到对应驱动。设备路径默认 `/dev/dri/renderD128`，多 GPU 或多渲染节点时可在后台改成实际节点。
- 硬件解码默认关闭。开启后会在输入前加入厂商 `-hwaccel`；源编码不被 GPU 解码器支持时会失败并回退到软件档，生产环境建议先在后台用“重新检测”和一次实际播放确认。

GPU 转码会占用宿主机 CPU 做解封装/音频编码；只开启 GPU 编码时，解码和缩放仍在 CPU。弱 NAS 上 4K HEVC 建议同时开启硬件解码并先用实际片源验证。

IP 归属地解析使用内置的 Hiofd 协议字段，不需要额外配置；字段不会写入日志、数据库或 API。Hiofd 不可用时管理员仪表盘仍显示客户端 IP，但归属地为空。

反向代理应向 Lux 传递 `X-Forwarded-For` 和 `X-Forwarded-Proto`；Lux 会优先使用这些头部中的客户端 IP 和协议。这两个头只用于展示与访问诊断：`X-Forwarded-Proto` 不参与认证、授权或 ACL，WebSocket 的同源校验也只比较 `Origin` 与 `Host`，不读取任何转发头。

## Nginx 反向代理

以下配置要点适用于把 `lux.example.internal` 转发到本机 8097 端口的 HTTPS 代理：

```nginx
server {
    listen 443 ssl;
    http2 on;
    gzip_static on;
    server_name lux.example.internal;

    ssl_certificate     /etc/letsencrypt/live/lux.example.internal/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/lux.example.internal/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8097;
        proxy_http_version 1.1;
        # 保留浏览器实际使用的 host[:port]。WebSocket 握手只比较 Origin 的 host[:port] 与这个头，
        # 写成 $host 会丢掉非默认端口，所有握手都会被 400 拒绝。
        proxy_set_header Host $http_host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket 升级（ADR-054 D12）：两行缺一不可。
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";

        proxy_set_header Range $http_range;
        proxy_set_header If-Range $http_if_range;
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 1h;
        proxy_send_timeout 1h;
    }
}
```

Lux 已为 `web/dist` 下的 JS/CSS/SVG/HTML 生成并直接服务 `.gz` 预压缩文件（`content-encoding: gzip`、`vary: accept-encoding`）；`gzip_static on` 只让 nginx 复用这些文件，不会在运行时重新压缩。HTTP/2 由 nginx 终止 TLS 时提供，`luxd` 自身仍监听明文 HTTP/1.1。

代理应保留 `206`、`Content-Range`、`Content-Length`、`Accept-Ranges` 和 `ETag`。不能把媒体流改成缓存整文件的代理模式。

### WebSocket 升级（`/api/v1/ws` 与 `/embywebsocket`）

Lux 的 Web 实时通道是单端点 `GET /api/v1/ws`，Emby 兼容的 `/embywebsocket` 保留；两条路径共用同一套升级与连接管理，`location /` 里的两行升级头对它们同时生效，不需要单独开 location。反代侧必须满足：

- `proxy_http_version 1.1;` 必须保留。`http2 on` 只影响浏览器到 nginx 的一段，nginx 到 `luxd` 仍是经典 HTTP/1.1 Upgrade，不需要 RFC 8441。
- `proxy_set_header Upgrade $http_upgrade;` 与 `proxy_set_header Connection "upgrade";` 缺一不可（ADR-054 D12）。nginx 默认不转发这两类逐跳头，缺了它们握手会以普通 `GET` 到达 Lux。
- `Host` 必须是 `$http_host`，**不是** `$host`。Lux 只比较 `Origin` 的 host[:port] 与 `Host` 头，不比较 scheme，也不读取任何转发头；`$host` 会丢掉非默认端口（`https://lux.example.internal:8443` 变成 `lux.example.internal`），于是每次握手都被 `400` 拒绝。非默认端口的部署尤其要检查这一行。
- `proxy_buffering off;` 保留，帧才能即时下发；`proxy_read_timeout` 必须明显大于 25 秒的服务端心跳（样例的 1 小时足够）。
- WS 握手是 `GET`，**不要求也不接受** CSRF 令牌；等价防护是会话 cookie 的 `SameSite=Lax` 加服务端的同源校验。因此不要把 nginx 配置成改写或补上 `Origin`。

### 缺头、改写 Host 与排查

- **缺 `Upgrade`/`Connection`**：握手被当作普通 `GET` 转发，`/api/v1/ws` 不会升级（ADR-054 的端点合同要求显式返回 `426 Upgrade Required`；同一 ADR 的「反向代理要求」一节仍写着 Axum 提取器默认的 `400`，该默认行为已被端点合同明确排除，以 `426` 为准）。浏览器侧两种情况都只看到 `close 1006`——**与 401 完全无法区分**。前端随后静默落到既有轮询（用户面 30 秒，管理面 2 s/5 s/15 s），页面看起来“功能正常”，管理页负载反而高于断线基线。这是本次迁移最可能的失败点。
- **服务端日志的拒绝分类**：握手被拒时 Lux 只记录原因分类（`origin-missing` / `origin-mismatch` / `sec-fetch-site` / `unauthenticated` / `budget`），不记录头原值。看到 `origin-missing`/`origin-mismatch` 说明请求已经是升级请求但来源校验没过；看不到任何握手拒绝分类、只看到该路径的普通 `GET`（部署开启请求日志时），说明升级头没到 Lux。`Origin` 只在 `debug` 级记录。
- **反代改写了 `Host`**（例如改成上游 `127.0.0.1:8097`）：`Origin` 与 `Host` 不再一致，**所有握手都会被 `400` 拒绝**，前端落到轮询。修正在反代侧（`proxy_set_header Host $http_host;`），不是放宽校验：Lux 没有 `LUX_ALLOWED_ORIGINS` 之类的来源白名单，也不会用 `X-Forwarded-Proto`、`Forwarded`、`X-Forwarded-Host` 判断来源。
- **反代补了 `Sec-Fetch-Site: cross-site`**（或中间层 WAF 会改写该头）：Lux 一律拒绝握手，这属于中间层行为，不在 Lux 配置里放宽。
- **`101` 之后收不到帧**：检查 `proxy_buffering off;` 是否被别的配置覆盖，以及 `proxy_read_timeout` 是否短于 25 秒心跳。

手工复现握手时，先直连上游、再经反代，两次对比（`curl` 在 `101` 之后会一直等数据，`--max-time` 结束属正常）：

```bash
# ① 直连 luxd（绕过反代）：期望 HTTP/1.1 101
curl -i --http1.1 --max-time 3 http://127.0.0.1:8097/api/v1/ws \
  -H 'Host: lux.example.internal' \
  -H 'Origin: https://lux.example.internal' \
  -H 'Cookie: lux_session=<浏览器里的会话 cookie>' \
  -H 'Upgrade: websocket' -H 'Connection: Upgrade' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ=='

# ② 经反代：同上换地址与 Host；得到 101 说明升级已被正确转发
curl -i --http1.1 --max-time 3 https://lux.example.internal/api/v1/ws \
  -H 'Origin: https://lux.example.internal' \
  -H 'Cookie: lux_session=<浏览器里的会话 cookie>' \
  -H 'Upgrade: websocket' -H 'Connection: Upgrade' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ=='
```

读结果的方式（两条命令都带真实 cookie，鉴权不会掩盖升级判断）：`502`/`504` 是反代到上游不通；`400`/`426` 说明请求没有以合法升级到达 Lux（先查两行升级头与 `Host`）；`401` 是凭据缺失或无效（cookie 过期、没带上），不带真实 cookie 时它不能证明升级是否被转发；`101` 才是升级成功。缺少 `Origin` 的 cookie 握手同样会被拒绝，具体码取决于实现（`400` 或 `401`），属于来源校验而不是反代故障。浏览器里永远只看到 `1006`，所以判断必须落在服务端与反代两侧。不要为了排障把会话 cookie 写进日志或工单。

### Caddy 与其它反向代理

仓库里没有 Caddy 配置，该链路**未实机验证**。按 Caddy 文档，`reverse_proxy` 默认透传 `Upgrade`/`Connection` 并保留原始 `Host`，通常不需要额外指令；仍需确认没有 `header_up Host {upstream_hostport}` 之类的改写，并按上面的 `101` 对照步骤实测。Tailscale Serve 等隧道工具同理：必须保留原始 `Host` 与 `Upgrade`，不要让中间层补写 `Origin`。

## Tailscale

Tailscale Serve 或同类 HTTPS 转发应只把本机 Lux HTTP listener 映射到 tailnet 内部，不要使用公开 Funnel，除非额外配置身份访问策略。转发目标为：

```text
http://127.0.0.1:8097
```

启用后从另一台 tailnet 设备验证：

1. HTTPS 证书和浏览器安全锁正常。
2. 初始化完成后登录、退出和 CSRF 请求正常。
3. MP4 `GET` 返回 `206`，Range、`Content-Range` 和 `Content-Length` 保留。
4. WebSocket 升级与长连接正常：`/api/v1/ws` 与 Emby 客户端的 `/embywebsocket` 都能完成 `101` 握手，且不因反代的短超时被切断（本条是检查项，不是结论；Serve 类转发是否透传 `Upgrade` 与原始 `Host` 必须在目标设备上按「缺头、改写 Host 与排查」的步骤实测）。
5. 反代转发的客户端 IP 和 HTTPS 协议头被 Lux 正确识别（这两个值只用于展示与访问诊断，不参与认证、授权、ACL 或 WebSocket 同源校验）。

## 升级与回滚边界

发布镜像使用不可变版本标签和 digest，不使用 `latest` 作为唯一发布标识。应用镜像通过 `docker-bake.hcl` 引用固定版本的 `lux-runtime:trixie-jellyfin-ffmpeg7-v1`，CI 在构建时进一步锁定该架构的 manifest digest；普通应用更新只会产生新的 `luxd` 和 Web 层，Docker 会按 layer digest 复用 runtime 层。只有 runtime/Dockerfile 或 Jellyfin FFmpeg 依赖发生变化时，才提升 `RUNTIME_IMAGE_TAG` 并生成新的 runtime 镜像。推荐使用和 CI 相同的构建图：

```bash
docker buildx bake --load \
  --set app.platform=linux/arm64 \
  --set app.args.LUX_VERSION=0.2.7 \
  --set app.tags=lux:0.2.7 \
  app
# 如果使用本地构建的镜像，请先将 compose.yaml 中 lux.image 指向 lux:0.2.7
docker compose up -d --force-recreate lux
```

启动时会自动执行当前已选择数据库的 migrations；升级前应停止写入并同时保留 `/config` 与 `/media` 的宿主机目录。当前版本不提供应用内备份/恢复或跨数据库迁移工具，也不提供 SQLite 与 PostgreSQL 之间的数据迁移；正式 NAS 发布前必须由运维侧完成配置目录、媒体目录和（如使用）PostgreSQL 数据库的快照与恢复演练。

### 数据库文件缺失时的启动保护

首次成功启动后，Lux 会在配置目录写入 `server-state.json`（只有 `serverId` 与写入时间，不含凭据）。之后每次启动都会在打开数据库之前比较这两者：如果标记存在，但数据库文件缺失或其中没有 `lux_meta.server_id`，Lux 拒绝启动并打印恢复指引，而不会静默创建一个空库再跑一遍 migration。这条保护针对的正是「只恢复了 `/config`、数据卷没挂载、`lux.db` 被误删」这类故障；没有它，界面上会出现安装向导，看起来像一个新服务器，而真实数据仍在别处。

处理方式：

- 数据其实还在：把 `lux.db`（以及同一目录下的 `lux.db-wal`）恢复到原位置后重启。Lux 正常关闭时会截断 WAL，所以正常停止后拷贝 `lux.db` 单文件即可获得完整数据库。
- 确实要从空库重新开始：先确认旧的数据库与备份都不再需要，删除 `/config/server-state.json` 后重启；Lux 会视为全新安装，重新生成 `serverId`，原有客户端令牌与设备绑定随之失效。

标记文件损坏（非 JSON）时按「已初始化」处理并记录警告，宁可停机也不覆盖可能存在的真实数据。

插件兼容性：runtime target 使用 Debian Trixie，以满足当前官方 Linux 插件包的 glibc 要求。只替换 `/config/plugins` 中的插件包不会升级容器运行时；升级到包含此修复的 Lux 镜像后，应重新创建容器并确认插件进程能够启动：

```bash
docker buildx bake --load \
  --set app.platform=linux/arm64 \
  --set app.args.LUX_VERSION=0.2.7-plugin-runtime \
  --set app.tags=pdzhou/lux:0.2.7-plugin-runtime \
  app
# 将 compose.yaml 中 lux.image 临时改为 pdzhou/lux:0.2.7-plugin-runtime
docker compose up -d --force-recreate lux
docker exec lux sh -c 'uname -m; command -v ffprobe; command -v ffmpeg'
```

如果使用已发布镜像，应先拉取包含该 Dockerfile 变更的版本标签，再执行 `docker compose up -d --force-recreate lux`；不要仅依赖 `latest` 标签判断镜像是否已更新。

升级后的验收最少包括：

```bash
curl --fail http://127.0.0.1:8097/health/live
curl --fail http://127.0.0.1:8097/health/ready
docker compose ps
```

随后用真实客户端执行登录、媒体库列表、详情和一次 Range 播放。真实代理、NAS 7 天运行和发布签名仍需在目标环境单独记录，不能用本机 ARM64 结果替代。

## 本机故障注入

下面这些脚本（`disk-write-fault-smoke.sh`、`mount-loss-smoke.sh`、`proxy-smoke.sh`、`probe-smoke.sh`、
`restart-recovery-smoke.sh`）都按当前的初始化契约运行：

- 全新的配置目录启动后处于**数据库选择**模式（`POST /setup/complete` 会返回 `409
  DATABASE_CONFIGURATION_REQUIRED`），脚本先调用 `POST /api/v1/setup/database/select` 选定 SQLite。
- `POST /api/v1/setup/complete` 成功后实例会**自己重启**（`docs/SETUP-RESTART-DESIGN.md`），脚本通过
  `scripts/lib/wait-for-restart.sh` 观察一次连接失败再等到 `/health/ready` 恢复，而不是 `sleep` 猜时间。
- 该重启会取消 setup 启动的首轮扫描并在重启后重排，所以脚本不再沿用 `201` 里的 `scanJob`（任务已取消；
  `library.id` 不变）。需要自己发起或接管扫描的 4 个脚本——`mount-loss-smoke.sh`、`probe-smoke.sh`、
  `proxy-smoke.sh`（各自 `POST .../reconcile`）与 `restart-recovery-smoke.sh`（接管重排后的任务）——都会先等
  重排的首扫结束或先查到它，否则扫描端点会返回 `409`（已有任务运行）；`proxy-smoke.sh` 与其他脚本一样用
  `lux_wait_for_first_scan_settled` 完成这一步。
- `restart-recovery-smoke.sh` 因此验证的是当前的真实契约：对重排后的首扫执行 `kill -9` → 重启把该任务标为
  `CANCELLED`/`SERVER_SHUTDOWN` → 启动恢复排入替换任务 → 替换任务完成并写入 `libraries.lastScanAt`。它不再
  断言“同一个任务 ID 续跑”，因为没有通用的扫描续跑流程，任务会被替换。

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

本机还可以用临时自签名证书和 Nginx 反代演练 HTTPS 和 Range 响应头：

```bash
LUX_IMAGE=lux:arm64-local ./scripts/proxy-smoke.sh
```

该脚本验证转发公网地址和 `206`、`Content-Range`、`Content-Length`、`Accept-Ranges`、`ETag` 的保留，并额外断言 `/api/v1/ws` 经 nginx 的升级链路：带 cookie 的同源握手返回 `101` 与首帧 `{"type":"ready","version":1}`，缺少 `Origin` 的 cookie 握手与跨源握手被拒，普通 `GET` 不升级。这个端口是非默认端口，所以它同时是 `Host: $http_host` 的回归测试（写成 `$host` 时握手会因端口丢失被 `400` 拒绝）。运行前脚本先跑 `scripts/check-proxy-config.sh` 做不依赖 nginx/docker 的静态检查；冒烟本身需要构建里已经包含 LUX-894 的 `/api/v1/ws`。自签名证书和本机 Docker 网络不替代真实 Tailscale/HTTPS 实机验证。

扫描完成后可以用 ARM64 容器端到端验证媒体探测和 Emby 播放信息：

```bash
LUX_IMAGE=lux:arm64-local ./scripts/probe-smoke.sh
```

该脚本生成有效 MP4，验证扫描自动运行 `ffprobe`、媒体源进入 `READY`，并确认 `PlaybackInfo` 返回运行时长、媒体流和 `PROBE_COMPLETED` 事件；它不替代真实 NAS 媒体库验收。
