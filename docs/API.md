# Lux API（当前实现）

Lux 自有 API 使用 `/api/v1`，响应字段使用 camelCase。错误统一为：

```json
{
  "error": {
    "code": "AUTHENTICATION_REQUIRED",
    "message": "需要登录",
    "requestId": "..."
  }
}
```

## 初始化

- `GET /api/v1/setup/status`：返回 `initialized`。
- `POST /api/v1/setup/complete`：仅在没有用户时创建首个管理员；成功返回 201，重复或并发失败返回 `SETUP_ALREADY_COMPLETED`。

请求体至少包含 `username` 和 `password`，可选 `displayName` 和首个媒体库信息。初始化接口不接收 TMDb 配置；TMDb API Key 在插件详情页配置。密码只以 Argon2id PHC 哈希形式写入数据库。

## Web 会话

- `POST /api/v1/auth/login`：校验用户名和密码，成功后设置 `lux_session` 与 `lux_csrf` cookie。
- 远程请求只需通过用户认证和媒体库 ACL；Lux 不再依据来源 IP 或 `can_remote_access` 阻止反代后的请求。
- 登录失败按来源和用户名限流；失败响应不区分用户不存在、密码错误或暂时封锁。
- `GET /api/v1/auth/me`：读取当前 Web session，返回用户和权限。
- `POST /api/v1/auth/logout`：需要有效 `lux_session` 和 `X-CSRF-Token`，成功返回 204 并撤销 session。

`lux_session` 为 `HttpOnly; Secure; SameSite=Lax; Path=/`，数据库只保存其 SHA-256 哈希。`lux_csrf` 不设置 HttpOnly，供同源 Web 客户端读取并通过 `X-CSRF-Token` header 发送；数据库保存 CSRF 哈希。session 有效期为 30 天，注销后立即失效。

当前阶段的 cookie 始终标记 `Secure`，部署时应使用 HTTPS；本机 HTTP 集成测试只验证协议和服务端行为，不代表浏览器会在不安全来源发送 Secure cookie。

## 媒体库管理（LUX-030）

以下接口要求有效 Web session；写操作还要求 `X-CSRF-Token`，并检查当前用户的 `canManageServer` 权限：

- `GET /api/v1/admin/libraries`：列出媒体库及其根路径。
- `POST /api/v1/admin/libraries`：创建媒体库。请求体为 `{ "name": "Movies", "kind": "MOVIE", "scraperId": "tmdb" }`，`kind` 支持 `MOVIE`、`SERIES`、`MIXED`；实时监听默认开启，不再提供关闭选项。历史客户端发送的 `realtimeWatchEnabled` 字段会被兼容接受但不会关闭监听。`scraperId` 可省略或为 `null`，表示不进行在线刮削，但仍读取本地 NFO 和图片。
- `PATCH /api/v1/admin/libraries/{libraryId}`：运行时更新全量校验/元数据 cron 计划、扫描/探测并发、`scraperId` 和媒体库策略覆盖。实时增量扫描由文件系统监听触发，不接受计划配置。历史 `realtimeWatchEnabled` 字段会被兼容接受但始终按 `true` 处理；历史 `incrementalSchedule` 字段也会被兼容接受但始终为 `null`。字段均可省略；计划、`scraperId` 和 `mediaStrategy` 使用 `null` 清空，计划表达式必须是标准五段式 cron（分 时 日 月 周），最长 128 个字符；并发范围为 1-64。例如 `{ "scraperId": "tmdb", "reconciliationSchedule": "0 3 * * *", "metadataSchedule": "*/5 * * * *" }`。修改无需重启，下一次调度轮询读取最新配置；刮削器必须已安装且配置完成。
- `POST /api/v1/admin/libraries/{libraryId}/roots`：添加根路径。请求体为 `{ "path": "/media/movies" }`；成功后自动创建异步扫描任务并返回 `scanJob`，扫描完成后若配置刮削器会继续自动匹配元数据。
- `PATCH /api/v1/admin/users/{userId}/libraries/{libraryId}`：授予或撤销普通用户访问媒体库。请求体为 `{ "canView": true }`，需要管理员 Web session 和 CSRF。
- `POST /api/v1/admin/libraries/{libraryId}/scan`：创建并异步执行分批扫描任务，返回 202 和 job 状态。
- `POST /api/v1/admin/libraries/{libraryId}/reconcile`：按当前库配置创建并异步执行一次调和扫描；已停用或不存在的媒体库返回 404。
- `POST /api/v1/admin/jobs/{jobId}/cancel`：请求取消扫描任务，返回 202。
- `GET /api/v1/admin/jobs?page=1&pageSize=50&status=FAILED`：管理员分页查看扫描任务，可按 `PENDING`、`RUNNING`、`COMPLETED`、`CANCELLED` 或 `FAILED` 过滤。
- `GET /api/v1/admin/jobs/{jobId}/events?page=1&pageSize=100&level=ERROR&eventCode=SCAN_IO`：查看单个任务的结构化生命周期日志，支持级别和稳定事件代码筛选；页大小限制为 1-100。
- `POST /api/v1/admin/jobs/{jobId}/retry`：重试已失败或已取消的扫描任务，创建新的扫描任务并返回 202。
- `GET /api/v1/admin/scheduled-tasks?page=1&pageSize=100`：分页查看所有已注册的任务，包含 `ownerType`、媒体库名称、`taskType`、`name`、`description`、`sourceType`、可空 `pluginId`、`schedule`、启用状态、资源限制和更新时间；结果也包含已停用或尚未配置计划的注册项。
- `PUT /api/v1/admin/scheduled-tasks`：只修改已注册任务的 cron 计划。媒体库任务使用 `{ "ownerType": "LIBRARY", "ownerId": "...", "taskType": "RECONCILIATION_SCAN|METADATA_PARSE", "schedule": "0 3 * * *", "isEnabled": true }`；全局 STRM 任务使用 `{ "ownerType": "GLOBAL", "ownerId": "global", "taskType": "STRM_MEDIA_INFO", "schedule": "0 3 * * *" }`。媒体库任务传 `schedule: null` 或 `isEnabled: false` 会清空计划；STRM 任务的计划必须非空，并会同步回插件配置。实时增量扫描（`INCREMENTAL_SCAN`）由文件系统事件触发，不属于此接口管理范围。不存在的注册项返回 404，不会因为管理请求凭空创建任务。写操作需要管理员 Web session 和 CSRF，并与对应的媒体库或插件配置保持同一份配置。Lux 按 UTC 解释 cron 表达式。
 - `POST /api/v1/admin/strm-probe-jobs`：按 `org.lux.strm-media-info` 已保存的插件配置创建并异步执行 STRM 媒体信息/缩略图任务，返回 202 和按库拆分的任务；不从请求体读取媒体库或并发配置，也不返回 URL。
- `GET /api/v1/admin/strm-probe-jobs?page=1&pageSize=50&status=FAILED`：分页查看 STRM 探测任务，状态支持 `PENDING`、`RUNNING`、`COMPLETED`、`CANCELLED` 和 `FAILED`。
- `GET /api/v1/admin/strm-probe-jobs/{jobId}`：查看单个 STRM 探测任务的状态、进度、并发、旁车开关和安全错误摘要。
- `POST /api/v1/admin/strm-probe-jobs/{jobId}/cancel`：请求取消 STRM 探测任务，返回 202；worker 不再领取新媒体源。
- `POST /api/v1/admin/strm-probe-jobs/{jobId}/retry`：重试失败或已取消的 STRM 探测任务，返回新的任务并异步执行 202。
- `PUT /api/v1/admin/plugins/org.lux.intro-outro-detector/config`：只保存片头片尾插件的并发、开头/结尾窗口、匹配阈值和 Cron 参数；媒体库归属由 `chapterSourceId` 保存。旧 `libraryIds` 配置仅用于兼容迁移，不再作为调度事实来源。
- `POST /api/v1/admin/libraries`：创建媒体库时可传 `chapterSourceId`；该字段仅剧集库或混合库可设置，省略或为 `null` 表示关闭片头片尾来源。
- `PATCH /api/v1/admin/libraries/{libraryId}`：可更新 `chapterSourceId`。该字段仅剧集库或混合库可设置；值必须来自已安装、已启用且配置完整并声明 `chapters.detect` 或 `chapters.lookup` 的插件；传 `null` 清除选择。混合库仅对剧集/分集检测，运行时输出只返回当前库所选来源的章节标记，切换来源不会删除历史标记。
- `GET /api/v1/admin/chapter-sources?page=1&pageSize=50`：分页列出可供媒体库选择的片头片尾数据源。
- `POST /api/v1/admin/libraries/{libraryId}/chapter-detection`：管理员立即启动该媒体库的片头片尾检测任务，可选覆盖插件 ID、并发、窗口、阈值和 `forceRefresh`；不读取或探测容器普通章节。自动任务由媒体库选中的章节来源注册：本地音频检测默认每周运行，TheIntroDB 在线来源默认每天运行。
- `GET /api/v1/admin/chapter-detection-jobs?page=1&pageSize=50&status=FAILED`、`GET /api/v1/admin/chapter-detection-jobs/{jobId}`：分页查看或读取检测任务。
- `POST /api/v1/admin/chapter-detection-jobs/{jobId}/cancel`、`POST /api/v1/admin/chapter-detection-jobs/{jobId}/retry`：取消或重试检测任务；写操作需要管理员 Web session 和 CSRF。
- `POST /api/v1/admin/libraries/{libraryId}/danmaku/match`：管理员提交 `{ "concurrency": 2, "overwrite": false }`，为已索引的本地视频创建异步弹幕匹配任务；成功返回 202，默认不覆盖已有同名 XML。
- `GET /api/v1/admin/danmaku/match-jobs?page=1&pageSize=50&status=FAILED`：分页查看弹幕匹配任务；详情、取消和重试分别使用 `/api/v1/admin/danmaku/match-jobs/{jobId}`、`/cancel` 和 `/retry`。
- `GET/PATCH /api/v1/admin/settings`：读取或调整 `serverName`、兼容保留的 `resumePlayedPercent`、`resumeMinTicks`（非负）和 `mediaStrategy`。自动标记已看的个人阈值请使用 `/api/v1/auth/settings`；`resumePlayedPercent` 不再控制个人自动已看或继续观看。`serverName` 会去除首尾空格，长度限制为 1-80 个字符，不接受控制字符。媒体策略的图像开关为 `poster`、`logo`、`thumbnail`、`banner`、`disc`、`artwork`、`wallpaper`，另有元数据/图片语言、地区、默认刮削器、`metadataRefreshMode`（`FILL_MISSING` 或 `FULL_REFRESH`）、最大背景图数量、最小下载宽度、字幕默认值和 `applyScope`（`NEW_CONTENT`、`SELECTED_CONTENT`、`ALL_CONTENT`）。旧策略 JSON 缺少新增字段时默认按 `FILL_MISSING` 处理。网络代理设置通过 `networkProxyUrl` 写入，支持 `http`、`https`、`socks4`、`socks4a`、`socks5` 和 `socks5h`；传 `null` 清除。响应中的 `networkProxy` 只返回脱敏地址、是否配置认证、来源和重启提示，不返回认证信息。写操作需要管理员 Web session 和 CSRF，响应不包含任何插件凭据。
- `GET/PATCH /api/v1/auth/settings`：读取或调整当前登录用户的 `playedPercent`（1-100），默认 95；写操作需要当前用户 Web session 和 CSRF。该设置只影响当前用户的自动已看阈值。
- `GET/PATCH /api/v1/auth/library-order`：读取或调整当前登录用户的媒体库顺序；写入 `{ "libraryOrder": ["<libraryId>", ...] }`，只接受当前用户可访问的媒体库 ID，未提交的可访问媒体库稳定追加到末尾；写操作需要当前用户 Web session 和 CSRF。该顺序同时用于 Lux Web 和 Emby `Views`、虚拟根目录、`VirtualFolders` 及 `OrderedViews`。
- 弹幕插件 `org.lux.danmaku` 的配置通过通用 `PUT /api/v1/admin/plugins/{pluginId}/config` 保存；`providerBaseUrl` 只在插件配置响应中以脱敏值展示，主设置页不再保存弹幕配置。
- `POST /api/v1/admin/settings/network-proxy/test`：管理员检测当前输入或已生效的网络代理；服务端只请求 TMDb、百度、Google 和 Cloudflare 四个固定目标，返回逐站延迟/HTTP 状态、网络出口 IP 和 Cloudflare 返回的两位国家/地区代码。需要管理员 Web session 和 CSRF；认证信息不会出现在响应或日志中。
- `GET /api/v1/admin/health`：返回管理员可见的运行诊断，包括 schema、SQLite WAL 与实际写探针结果（`database.status`、`database.writable`）、配置目录实际写入能力、ffprobe、TMDb、媒体库根路径和后台任务计数；同时返回 `runtime.seconds`、`resources.cpu`、`resources.memory` 和 `resources.mediaStorage`。CPU/内存只读取 Lux 容器 cgroup，`mediaStorage` 只读取容器内 `/media` 挂载点的文件系统容量；不回退到宿主机整体资源，不返回本地配置路径或密钥。CPU 有 cgroup 配额时 `usagePercent` 按配额归一化到 0-100，无配额时按一个 CPU 核心为 100% 且可能超过 100%。指标不可用时 `available` 为 `false`，数值字段为 `null`。写入能力失败时整体 `status` 为 `degraded`，但仍返回可诊断的安全状态。
- `GET /api/v1/admin/dashboard`：返回仪表盘聚合数据，包括 `server`（名称、Lux 版本、commit 和 schema）、`stats`（已启用媒体库中未移除的 `movieCount`、`seriesCount`，以及未禁用用户的 `userCount`）、`health`、最多 24 个 `nowPlaying` 会话和最多 24 条 `activity`。正在播放数据只返回安全的媒体/轨道摘要、可空的 `remoteIp` 客户端来源 IP，以及可空的 `remoteIpLocation`（`location`、`district`、`street`、`isp`）。归属地只读取进程内缓存；首次遇到公网 IP 时由后台异步查询 Hiofd，失败或未完成时为 `null`，不返回服务器路径、外部播放 URL 或认证信息；接口要求管理员 Web session。
- `GET /api/v1/admin/events`：管理员 Web session 的 SSE 失效通知流，不要求 CSRF。响应为 `text/event-stream`、禁止缓存并关闭反向代理缓冲；首帧为 `event: ready` 与 `{"version":1}`，变更帧为 `event: invalidate` 与 `{"scope":"dashboard|jobs|libraries|plugins|users|metadata|settings|all"}`，每 15 秒发送注释心跳。广播丢帧时发送 `all`，客户端应重新读取所有管理员查询；流不传输业务数据或敏感信息。
- `GET /api/v1/admin/logs`：返回脱敏的管理员审计事件，支持 `page`、`pageSize`、`level` 和 `eventCode` 筛选。
- `GET /api/v1/admin/logs/export?from=YYYY-MM-DD&to=YYYY-MM-DD`：管理员按 UTC 日期导出持久化 JSON 日志；单日范围返回对应 `lux.YYYY-MM-DD.log` 原始文件，多日范围返回包含每日文件的 ZIP。不传日期时默认最近 7 个 UTC 日，日期范围最多 31 天，只读取 `/config/logs/lux.YYYY-MM-DD.log`。

## 插件与刮削器（LUX-142、LUX-162）

以下接口要求 `canManageServer`；写操作还要求 `X-CSRF-Token`：

- `GET /api/v1/admin/plugin-store`：读取当前插件商店来源和默认来源。默认来源为 `https://github.com/Qoo-330ml/Lux-plugins`，GitHub 仓库地址解析为 `main/index.json`；读取需要管理员权限。
- `PUT /api/v1/admin/plugin-store`：管理员发送 `{ "url": "https://example.com/lux/index.json" }` 保存插件目录来源，需要 CSRF。只接受无凭据、无 fragment、无控制字符且不超过 2048 个字符的 HTTPS 地址；成功返回 `url` 和 `defaultUrl`。
- `GET /api/v1/admin/plugins?page=1&pageSize=50`：分页返回当前插件商店目录和 `/config/plugins` 本地发现的插件包及 `installed`、`enabled`、`running`、`configured`、`available`、`configurable`、`configFields`、非敏感 `configValues`、`configSource`、`version`、`runtime`、`capabilities`、`status` 和脱敏 `lastError` 状态。`configFields` 包含输入类型、是否多选、默认值、数值范围和选项来源；`media-libraries` 选项由当前媒体库动态填充。TMDb 的 `configValues` 返回非敏感设置，不返回 API Key 或 Token。目录不可用时，已发现的本地插件仍可用于已安装管理页。
- `POST /api/v1/admin/plugins/{pluginId}/install`：安装本地发现的插件，或下载当前插件商店目录声明的 `.zip` 包并校验大小、路径、manifest、平台入口和 SHA-256 后原子写入 `/config/plugins`，默认启用。首次安装返回 201，重复请求返回 200；下载失败或未知插件返回相应错误，不改变安装状态。
- `PATCH /api/v1/admin/plugins/{pluginId}/enabled`：更新已安装插件的启用状态，请求体为 `{ "enabled": true }` 或 `{ "enabled": false }`。禁用只改变运行/选择状态，不删除安装记录；已安装管理列表仍会返回该插件。未安装或未知插件返回相应错误，成功返回更新后的插件状态。
 - `PUT /api/v1/admin/plugins/{pluginId}/config`：替换或更新插件配置。TMDb 请求体可包含 `{ "apiKey": "...", "preferredLanguage": "zh-CN", "languageFallbackEnabled": false, "fallbackLanguages": ["zh-SG", "zh-HK", "zh-TW"], "alternateApiEnabled": true, "apiBaseUrl": "https://api.tmdb.org" }`；`org.lux.strm-media-info` 请求体为 `{ "libraryIds": ["..."], "concurrency": 2, "mediaInfoEnabled": true, "thumbnailEnabled": false, "thumbnailPositionPercent": 30, "existingInfoPolicy": "SKIP", "writeSidecars": true, "schedule": "0 3 * * *" }`，其中 `thumbnailPositionPercent` 范围为 1-99，也支持 `OVERWRITE` 覆盖已有媒体信息。媒体信息和缩略图开关独立；启用截图时，没有本地或在线 `POSTER`/`THUMB` 的 STRM 也会截图，且同一截图同时作为 `POSTER` 和 `THUMB` 登记。媒体插件配置按 manifest 校验并保存到插件专属配置文件；成功返回不含明文凭据的插件状态。
- `POST /api/v1/admin/plugins/org.lux.strm-media-info/run`：按已保存的 strm-media-info 插件配置创建 STRM 探测任务，返回 202；不接受媒体库、并发等宿主覆盖参数。
- 插件包必须是 `.zip` 或开发用解压目录，根目录包含 `manifest.json`。Lux 启动时校验包格式、协议版本、平台架构、文件哈希和签名；校验失败的包不会运行。
- 插件通过独立进程和 JSON-RPC 风格协议提供 `plugin.hello`、`plugin.health`、`metadata.search`、`metadata.get`、`metadata.images`、`metadata.externalIds`、`metadata.trailers` 和 `plugin.shutdown`。
- `media_probe` 插件必须声明 `category: "MEDIA"` 和 `capabilities: ["media.probe"]`。`org.lux.strm-media-info` 的 `media.probe` 只处理单个由宿主校验的不透明 STRM 目标，可为媒体信息和缩略图分别设置开关；目标可以是私网地址、公网地址、域名或路径。宿主负责并发、超时、取消、恢复、落库和可选旁车写回。播放和 PlaybackInfo 不触发该 RPC。
- `chapter_detector` 插件必须声明 `category: "MEDIA"`，并至少声明 `chapters.detect` 或 `chapters.lookup`。`org.lux.intro-outro-detector` 使用 `chapters.detect` 比较宿主提取的有界音频指纹；`org.lux.theintrodb-chapter-source` 使用 `chapters.lookup`，只把已保存的 TMDb/TVDb/IMDb ID、季号、集号和可选时长交给在线插件，由插件访问固定的 TheIntroDB API。两者都不接收媒体路径或任务对象，宿主负责并发、超时、取消、恢复、结果落库和 Emby 章节映射。
- 未安装、未启用、无可用凭据、运行失败或未知的插件不能作为媒体库的 `scraperId`；选择不可用插件返回 `PLUGIN_UNAVAILABLE`。

## 通知器插件（LUX-183）

- `GET /api/v1/admin/notification-providers?page=1&pageSize=50`：分页返回已发现的
  `type: "notification"` 插件及其安装、启用、可用状态和配置字段。
- `GET/POST/PATCH/DELETE /api/v1/admin/notification-destinations`：继续兼容原有 Webhook 目标；创建或更新时可
  传 `providerPluginId` 和非秘密 `providerConfig`。省略时使用 `builtin.webhook`。外部 provider 不需要 URL，
  但 `secret` 仍只在创建/轮换响应中返回，不写入事件或普通列表。
- 通知插件必须声明 `notification.send`，宿主通过独立进程 RPC 投递；插件结果 `DELIVERED`、`RETRYABLE`、
  `FAILED` 由统一 outbox worker 处理超时、退避、恢复和失败记录。

插件包不从任意未登记的远程 URL 自动下载；远程安装只使用当前插件商店目录声明的 Release 包地址。插件 API、媒体库 API 和日志不返回插件配置中的敏感值；TMDb API Key 和 Read Access Token 只存在受限配置或外置插件运行时中。

## 元数据候选管理（LUX-053）

- `GET /api/v1/admin/metadata/pending?page=1&pageSize=50`：兼容接口，管理员分页查看 pending 候选；页大小限制为 1-100。Web 控制台不再提供独立元数据纠错页面，实际处理入口是媒体库的 `metadataStatus=PENDING` 筛选和媒体详情页。
- `GET /api/v1/admin/items/{itemId}/identify/candidates?q=关键词&page=1&pageSize=50`：管理员按 provider ID 或候选 JSON 搜索指定条目的 pending 元数据匹配候选，并返回 `fieldDiffs` 预览。
- `POST /api/v1/admin/items/{itemId}/identify/candidates`：管理员发送 `{ "query": "标题", "year": 2020 }`，通过条目所属媒体库的 `scraperId` 搜索元数据匹配候选；候选的 provider ID 必须来自当前选中的刮削器，最多写入 20 个带 24 小时过期时间的 pending 候选，并返回当前条目的候选页。需要 `X-CSRF-Token`；刮削器不可用或请求失败不会改变本地条目。
- `POST /api/v1/admin/items/{itemId}/identify/candidates/{candidateId}/select`：管理员选择元数据匹配候选并发送 `{ "mode": "fillMissing" }` 或 `{ "mode": "refreshUnlocked" }`，需要 `X-CSRF-Token`。前者只补空元数据字段和缺失图片，后者刷新未锁定字段和图片；候选中的每类图片只使用第一张，所属媒体库未启用的类型不写回，找不到的类型跳过；NFO/图片写回全部成功后才返回 `ONLINE_CONFIRMED`，失败返回可重试错误且候选保持 pending。
- `POST /api/v1/admin/metadata/reidentify`：管理员发送 `{ "itemIds": ["..."] }` 创建指定条目的候选搜索任务；条目去重后限制为 1-100 个，任务持久化为 `QUEUED/RUNNING/COMPLETED/FAILED`，需要 `X-CSRF-Token`。任务使用条目所属媒体库的刮削器获取对应 provider ID；路径中的 `reidentify` 为兼容标识，不自动确认候选。
- `GET /api/v1/admin/metadata/reidentify?page=1&pageSize=50&status=RUNNING`：管理员分页查看元数据匹配和元数据刷新任务；支持 `QUEUED`、`RUNNING`、`COMPLETED`、`FAILED`、`CANCELLED` 过滤，返回模式、`jobScope`、媒体库身份、进度和稳定错误代码；摘要只聚合当前页任务。
- `GET /api/v1/admin/metadata/reidentify/{jobId}`：管理员读取批量元数据匹配任务及逐条状态、候选数量和稳定错误代码；前端可按任务 ID 轮询。
- `POST /api/v1/admin/metadata/reidentify/{jobId}/cancel`：管理员请求取消 `QUEUED` 或 `RUNNING` 的元数据任务，需要 `X-CSRF-Token`，返回 202；worker 在当前批次完成后停止领取新条目并将任务标记为 `CANCELLED`。
- `POST /api/v1/admin/metadata/reidentify/{jobId}`：管理员对 `FAILED` 或 `CANCELLED` 任务重新排队未完成条目，保留已经成功的条目，需要 `X-CSRF-Token`；其他状态返回冲突。
- `POST /api/v1/admin/items/{itemId}/metadata/refresh`：管理员发送 `{ "mode": "FILL_MISSING" | "FULL_REFRESH" }` 刷新一个条目的元数据；创建一个持久化后台任务并立即返回。Web 端“刷新元数据”使用 `FILL_MISSING`：电影或单集刷新自身，季刷新该季及其单集，剧集刷新该剧、所有季和所有单集，因此会补写缺失的季海报和单集横向剧照，同时保留已有本地字段和图片。
- `POST /api/v1/admin/libraries/{libraryId}/reidentify`：管理员从媒体库入口发起整库元数据匹配；服务端创建一个持久化后台任务并立即返回 `{ "totalCount": 125, "job": { ... } }`。任务使用条目所属媒体库的刮削器，以 `FILL_MISSING` 自动选择高置信度最佳候选，内部最多 16 路并行处理条目，按媒体库图像策略下载图片并写回 NFO/图片；低置信度条目保留 pending 候选供后台处理。
- `POST /api/v1/admin/libraries/{libraryId}/metadata/refresh`：管理员发送 `{ "mode": "FILL_MISSING" }` 或 `{ "mode": "FULL_REFRESH" }`，每次媒体库操作创建一个持久化后台任务并立即返回。前者只补缺失的未锁定 NFO 字段和图片，后者刷新未锁定 NFO 字段并替换已有图片；锁定的 NFO 字段始终保留。任务内部最多 16 路并行处理条目，未配置刮削器时不发起在线请求并保留本地元数据。

根路径会先 canonicalize，再检查目录存在且可读；`isWritable` 独立返回。只读目录可以保存，但返回 `LIBRARY_PATH_NOT_WRITABLE` 警告。同一库的重复/重叠路径分别返回冲突/不可处理实体错误，跨库重叠返回结构化警告。

## Emby 认证（LUX-024）

- `GET /Users/Public`：返回未禁用用户的公开登录信息。
- `POST /Users/AuthenticateByName`：读取 `Username`/`Pw`，解析 `Authorization`、`X-Emby-Authorization` 或 `X-Emby-Authentication` 中的 `Client`、`Device`、`DeviceId`、`Version`，返回 `AccessToken`、`ServerId`，以及包含 `ServerId`、`Configuration`、`Policy` 等兼容字段的 `User` 和 `SessionInfo`。
- `POST /Sessions/Logout`：接受 `X-Emby-Token` 或 `api_key`，撤销对应 token，成功返回 204。
- `System/Info`：需要有效的 `X-Emby-Token` 或 `api_key`；`System/Info/Public` 和 `System/Ping` 不要求认证。

Emby access token 与 Web session 完全分离。access token 是高熵随机值，只在认证响应中返回；数据库只保存 SHA-256 哈希以及设备元数据。认证失败响应不区分“用户不存在”和“密码错误”。

## 共享管理员 API Key（LUX-182）

Lux 提供一个服务器级共享管理员 API Key，行为与 Emby API Key 兼容。所有拥有服务器管理权限的管理员看到同一个当前 Key；Key 调用按服务器管理员权限执行，不能区分具体管理员身份。

- `GET /api/v1/admin/api-key`：管理员 Web session 查看当前 Key。响应为 `{ "configured": true, "apiKey": "lux_..." }`；没有 Key 时 `apiKey` 为 `null`。需要管理员权限。
- `POST /api/v1/admin/api-key/rotate`：生成新 Key 并立即撤销旧 Key，需要管理员 Web session 和 `X-CSRF-Token`。
- `DELETE /api/v1/admin/api-key`：撤销当前 Key，需要管理员 Web session 和 `X-CSRF-Token`。
- API Key 认证支持 `X-Emby-Token`、`X-Lux-Api-Key`、`Authorization: Bearer <key>`，以及兼容的 `api_key` 查询参数。
- 共享 Key 同时适用于已实现的 `/api/v1` 和 Emby 兼容路由。Key 本身不能查看、轮换或撤销 API Key；这些操作必须使用 Web session。
- Key 持久化于 `/config/lux_admin_api_key` 的受限文件，至少使用 256 bit 随机熵。明文不会写入数据库、日志、审计事件或错误响应；轮换会使所有旧调用方立即失效。

## 当前边界

`GET /health/ready` 在数据库可读但事务写入探针失败时返回 503 和 `reason=database_write_unavailable`；`/api/v1` 的写入接口统一返回 `DATABASE_UNAVAILABLE` 错误契约并包含 requestId。

上述接口是 LUX-021/LUX-022 的基础能力。媒体库、Emby 兼容、用户管理和进度接口按开发规格后续任务逐项增加；未实现端点不应被客户端兼容性声明引用。

## 电影查询（LUX-034）

Lux 电影查询要求有效 Web session：

- `GET /api/v1/libraries`：返回已启用媒体库的基本信息，不暴露服务器路径。
- `GET /api/v1/libraries/{libraryId}/items?page=1&pageSize=50`：按稳定标题顺序分页返回条目；支持 `itemType`、`year`、`isPlayed`、`isFavorite`、`metadataStatus=PENDING`、`sortBy=Name|DateCreated|PremiereDate|CommunityRating` 和 `sortOrder=Ascending|Descending`（同时兼容下划线参数名），筛选、排序和分页在 SQLite 查询中完成；评分排序将无评分条目稳定放在有评分条目之后。`metadataStatus=PENDING` 返回仍有待确认候选的条目。
- `GET /api/v1/favorites?page=1&pageSize=50`：返回当前用户跨可见媒体库的收藏条目，按最近添加倒序分页；服务端执行用户状态和媒体库 ACL。
- `GET /api/v1/search?q=关键词&page=1&pageSize=50`：搜索标题、原标题和别名，结果执行媒体库 ACL。
- `GET /api/v1/home`：返回当前用户继续观看、推荐和可见媒体库入口；每个媒体库入口包含最多 12 条该库最新资源，按 `media_items.added_at` 倒序。所有内容均执行媒体库 ACL；响应中的 `recentlyAdded` 字段保留用于旧客户端兼容，Lux Web 首页按媒体库分别展示最新资源。
- `GET /api/v1/items/{itemId}/playback`：读取当前 Web 用户的播放位置、已看、收藏状态，以及该条目的活动播放状态（`state`、`isPaused`、`lastEventAt`）。
- `POST /api/v1/items/{itemId}/progress`：写入播放事件，需要当前 Web session 和 CSRF。请求体为 `{ "positionTicks": 1200000000, "durationTicks": 7200000000, "state": "PLAYING" }`；`state` 可为 `PLAYING`、`PAUSED` 或 `STOPPED`，省略时兼容为 `PLAYING`。
- `PUT /api/v1/items/{itemId}/favorite`：设置当前 Web 用户的收藏状态，需要当前 Web session 和 CSRF。
- `PUT /api/v1/items/{itemId}/played`：设置当前 Web 用户的已看状态，请求体为 `{ "played": true }`，需要当前 Web session 和 CSRF。
- `GET /api/v1/items/{itemId}`：返回电影详情、媒体源和已探测轨道。
- `GET /api/v1/items/{itemId}`：返回媒体详情、媒体源和已探测轨道；若已完成在线刮削，还返回 `rating`（0-10）、`ratingSource`、`premiereDate`、`lastAirDate`、`status`、`originalLanguage`、`providerIds`、`seasonCount` 和 `episodeCount`。剧集统计字段使用当前可见且未移除的季度/单集计算。
- `GET /api/v1/items/{itemId}/children?itemType=SEASON|EPISODE&seasonId=...`：Web 同源读取剧集季度/单集或合集成员，结果执行当前用户 ACL。媒体条目响应同时返回 `parentId`、`seriesId`、`parentIndexNumber`（季号）和 `indexNumber`（集号），用于保持剧集层级导航。
- `GET /api/v1/collections/{collectionId}`：返回可访问 BOX_SET 及按媒体库 ACL 过滤后的成员。
- `GET|POST /api/v1/admin/users`、`PATCH|DELETE /api/v1/admin/users/{userId}`：管理员管理用户、权限和禁用状态；删除为禁用语义，最后一个服务器管理账户受保护。
- `GET /api/v1/admin/users/{userId}/libraries`：读取该用户当前可访问的媒体库 ID，用于管理控制台展示 ACL；不返回服务器路径。
- `GET /api/v1/admin/audit?page=1&pageSize=50`：管理员分页读取管理操作审计事件。
- `GET /api/v1/admin/jobs/{jobId}`：管理员读取单个扫描任务详情，包括状态、进度、游标和错误。
- `GET /api/v1/admin/items/{itemId}/images`、`DELETE /api/v1/admin/items/{itemId}/images/{imageId}`：管理员查看图片索引并删除媒体根目录内的图片及索引；删除要求 CSRF，响应不暴露本地路径。
- `DELETE /api/v1/admin/items/{itemId}`：管理员删除指定媒体源及其同名旁车文件；若媒体文件已被外部删除，仍会清理 Lux 中的媒体源记录，没有其他媒体源时同时标记逻辑条目移除。支持通过 `sourceId` 选择版本，要求 CSRF。
- `GET /api/v1/auth/sessions`、`DELETE /api/v1/auth/sessions/{sessionId}`：当前用户查看并撤销其他 Web 会话；删除要求 CSRF，当前会话必须通过退出登录撤销。
- `GET|HEAD /api/v1/items/{itemId}/images/{type}`、`/{type}/{index}`：读取本地 poster/fanart，支持 ETag 和 `If-None-Match`。

Emby 目录查询要求有效 `X-Emby-Token` 或 `api_key`：

- `GET /Items/Counts`：返回当前用户可见媒体条目的 Emby 统计字段；支持 `UserId` 指定目标用户和 `IsFavorite=true|false` 按目标用户收藏状态过滤。Lux 当前支持电影、剧集、单集和合集计数，其余 Emby 类型返回 0；`ItemCount` 为过滤后所有可见目录条目（包含季度等层级条目）的总数。
- `GET /Library/VirtualFolders`：管理员获取 Emby 兼容的媒体库列表；返回完整的 `VirtualFolderInfo` 主要字段，包括 `Name`、`Locations`、`CollectionType`、`LibraryOptions`、`Id`、`Guid`、`ItemId`、`PrimaryImageItemId` 和刷新状态。`Id`、`Guid`、`ItemId` 使用同一个稳定的媒体库 ID，`LibraryOptions` 包含 `PathInfos`、`TypeOptions`、NFO/字幕/图片策略以及播放恢复阈值，并从 Lux 的全局或媒体库策略映射。支持根路径及 `/emby` 前缀，并接受共享 API Key。
- `GET /Persons?ParentId={libraryId}&Recursive=true&PersonTypes=Actor&StartIndex=0&Limit=50`：返回指定媒体库中去重后的演员列表，使用 Emby 风格的 `Items` 和 `TotalRecordCount`；顶层不额外返回 `StartIndex`。支持 `Fields`、`SortBy=Name|DateCreated`、`SortOrder=Ascending|Descending`，`Limit` 接受任意正整数且不设置服务端硬上限；演员项使用 `Type=Person`，并包含 `ServerId`、`ImageTags`、`BackdropImageTags`、`Name`、稳定 `Id`、`Role`、`DateCreated`、人物简介/生日等字段和可用的 `PrimaryImageTag`。`Recursive=true` 聚合媒体库所有后代条目，`Recursive=false` 只聚合直接子条目；未传 `Recursive` 时为兼容旧客户端按递归查询处理。支持根路径及 `/emby` 前缀，接受 Emby token 或共享 API Key；`ParentId` 必须是当前用户可访问的媒体库 ID，列表查询使用持久化人物关系索引，不在请求中扫描媒体目录。
- `GET /Users/{userId}/Views`：返回电影媒体库视图。
- `GET /Users/{userId}/Items/Root`、`GET /Items/Root?userId={userId}`：返回用户虚拟根目录；将该根 ID 作为 `ParentId` 并请求 `IncludeItemTypes=CollectionFolder` 时返回当前用户可见的媒体库文件夹。
- `GET /Users/{userId}/Items`、`GET /Items`：支持 `ParentId`、`Recursive`、`StartIndex`、`Limit`、`IncludeItemTypes` 和 `ExcludeItemTypes`，`ParentId` 可指向虚拟根、媒体库、物理媒体目录、剧集或季度；电影扫描会为物理媒体目录建立稳定的 `FOLDER` 条目，普通电影列表和统计不会把这些内部目录项当成电影。网易爆米花首页使用的无 `ParentId`、无 `Recursive=true`、无 `IncludeItemTypes` 但带 `ExcludeItemTypes` 请求返回当前用户可见的媒体库 `CollectionFolder`，再按媒体库 ID 请求 `Items/Latest`；递归查询按排除类型过滤；默认从 0 开始、每页 50 条，单页上限 100。
- `GET /Users/{userId}/Items`、`GET /Items`：另支持 `SearchTerm`、`IsPlayed`、`IsFavorite`、`Years`、`SortBy` 和 `SortOrder`，筛选后再分页；`SearchTerm` 使用标题、原始标题和别名搜索，并执行当前用户 ACL；`SortBy=DateCreated,SortName` 按 `DateCreated` 主排序并以标题稳定收尾。
- `GET /Users/{userId}/Items/{itemId}`、`GET /Items/{itemId}`：返回 Emby 兼容电影、剧集和季度详情 DTO；目录和详情条目使用标准 `SortName`、`SeasonId`、`IndexNumber`、`ParentIndexNumber`、`PremiereDate`、`ProviderIds` 和用户权限相关的 `CanDownload` 字段，旧客户端使用的 `Index` 别名继续保留；带 `Fields` 的列表按请求投影可选字段，缺失值不序列化为 JSON `null`，日期字段按 Emby 列表使用的 UTC ISO 时间格式输出；请求列表 `Fields=Chapters` 时返回章节数组（当前无本地章节数据时为空数组），章节元素使用 `StartPositionTicks`、`Name`、`MarkerType` 和 `ChapterIndex`。电影列表的 `ImageTags` 支持已索引的 `Primary`、`Logo`、`Thumb`、`Banner`、`Disc`、`Art` 和 `Wallpaper`，`BackdropImageTags` 保留所有背景图标签。文件夹类型返回 `ChildCount`/`RecursiveItemCount`，`UserData` 在没有播放位置时省略 `PlayedPercentage`，其余字段包含 `PlaybackPositionTicks`、`Played`、`IsFavorite` 和 `PlayCount`。
- `GET /Shows/{seriesId}/Seasons`：按用户媒体库权限返回季度。
- `GET /Shows/{seriesId}/Episodes?SeasonId={seasonId}&StartIndex=0&Limit=50`：返回剧集，可省略 `SeasonId` 获取整部剧集，支持分页。
- `GET /Users/{userId}/Items/NextUp`：按该用户的播放状态返回未看完单集。
- `GET /Shows/NextUp?UserId={userId}`：返回与 Emby 客户端兼容的用户未看完单集列表，支持 `StartIndex`、`Limit` 和 `Fields`。
- `GET|HEAD /Items/{itemId}/Images/{Type}`、`/{Type}/{Index}`：读取与 Lux API 相同的本地图片记录，支持 `X-Emby-Token` 或 `api_key`。
- `GET /api/danmu/{itemId}`：返回旁车 XML 的兼容信息和读取地址；支持 `option=Refresh`、`option=GetJsonById` 别名，但不会在客户端请求中访问上游。
- `GET /api/danmu/{itemId}/raw`：读取同目录同 basename 的有效 `.xml` 旁车，返回 `application/xml; charset=utf-8`；需要 `X-Emby-Token` 或 `api_key`，并执行媒体库 ACL。
- `GET /Users/{userId}/Items/Resume`：按用户播放位置、已看状态和服务器 Resume 阈值返回继续观看列表。
- `GET /Users/{userId}/Items/Latest`：按最近添加顺序返回当前用户可见媒体；`GroupItems` 默认开启，根目录或媒体库范围内默认只返回电影/剧集根条目，剧集/季度结果按剧集聚合并返回 `ChildCount`，传 `GroupItems=false` 可通过 `ParentId` 获取剧集单集。
- `GET /Search/Hints?SearchTerm=关键词&StartIndex=0&Limit=50`：返回 Emby 搜索提示，结果执行当前用户 ACL。
- `GET|HEAD /api/v1/items/{itemId}/subtitles/{streamIndex}`：读取指定外挂字幕流；需要 Web session，并执行媒体库 ACL。
- `GET|HEAD /api/v1/items/{itemId}/stream`：读取默认本地媒体源；可通过 `sourceId` 选择媒体源，需要 Web session 和媒体库 ACL。
- `GET|HEAD /Videos/{itemId}/{mediaSourceId}/Subtitles/{streamIndex}/Stream`：按指定媒体源读取外挂字幕。
- `GET|HEAD /Items/{itemId}/Subtitles/{streamIndex}/Stream`：按条目读取默认媒体源的外挂字幕。
- `GET|HEAD /Videos/{itemId}/stream`、`/Videos/{itemId}/stream.{container}`：读取默认媒体源；同时接受客户端常用的小写 `/videos` 变体。
- `GET|HEAD /Videos/{itemId}/{mediaSourceId}/stream`、`/stream.{container}`：读取指定媒体源；本地源返回文件流，HTTP `.strm` 源返回临时重定向到首个非空外部 URL，路径型/其他 `.strm` 源按解析器结果返回 `307 Location`。
- `GET|HEAD /Items/{itemId}/Download`：需要 `can_download` 和媒体库 ACL，返回所选单个媒体源的附件下载流；不打包同目录旁车文件。`mediaSourceId` 可选择源，`LOCAL_FILE` 直接读取库内文件，`STRM_URL` 读取 `.strm` 的首个非空 URL 后由 Lux 流式转发远程资源。
- `GET|HEAD /api/v1/items/{itemId}/download`：Lux 下载端点，需要 Web session、`can_download` 和媒体库 ACL；返回所选单个媒体源，不打包 ZIP。`sourceId` 可选择源；本地源直接流式读取，`.strm` 读取首个非空远程 URL 并由 Lux 请求、流式转发该资源，不返回 `.strm` 文本。
- `GET|POST /Items/{itemId}/PlaybackInfo`：返回可访问媒体源、媒体流、DirectPlay 能力和服务端生成的 `PlaySessionId`；支持 `MediaSourceId` 显式选择，支持 DirectPlay/DirectStream，不声明转码。每个媒体源可带 `Edition`/`Quality` 版本标签。
- 本地媒体源的 `MediaSources.Container` 使用实际文件扩展名（例如 `mkv`、`mp4`），不暴露 ffprobe 的复合 `format_name`。`DirectStreamUrl` 通过 `MediaSourceId` 定位源；`stream.{container}` 的后缀仅作兼容性后缀，服务端仍按媒体源记录读取文件。
- `MediaSources.Path` 对 `.strm` 源返回旁车记录中的外部媒体地址；`MediaStreams` 除基础轨道字段外，还返回旁车中的分辨率、画面比例、码率、色深、帧率、Profile、像素格式、声道布局和采样率等已验证字段。
- `MediaStreams` 不返回 Matroska/MP4 中标记为 `attached_pic` 的封面附加图轨，避免客户端将封面误认为可播放视频轨。
- `GET /Items/{collectionId}/Children`：返回按当前用户媒体库权限过滤的合集成员。

`.strm` 媒体源在 PlaybackInfo 中以 `Protocol=Http`、`IsRemote=true` 返回；`Path` 保存原始绝对上游地址，`DirectStreamUrl` 缺省（`null`），与普通 Emby 服务器一致，客户端按标准 `/Videos/.../stream` 端点发起播放。该端点对 HTTP `.strm` 返回 `307 Location` 到首个非空外部 URL，让客户端自行跟随上游重定向；路径型/其他目标由解析器处理并返回 `307 Location`。具有媒体库访问权限的客户端仍可能获得包含令牌的上游地址，因此 URL 中的令牌仍按产品设计明文保存和返回。

- `GET /Sessions`：返回当前用户的活动播放会话；管理员可查看全部活动会话。每个会话按 Emby 兼容字段返回 `Client`、`DeviceName`、`DeviceId`、`DeviceType`、`ApplicationVersion` 和 `RemoteEndPoint`；无法获得的值为 `null`。
- `POST /Sessions/Playing`、`/Sessions/Playing/Progress`、`/Sessions/Playing/Stopped`：幂等记录播放事件，并将位置单调写入用户状态；事件体中的设备/客户端字段优先，缺失时从上述认证头回填。
- `GET /api/v1/items/{itemId}/playback`：读取当前 Web 用户的播放状态和该条目的活动会话状态。
- `POST /api/v1/items/{itemId}/progress`：写入当前 Web 用户的播放开始、进度、暂停或停止事件；与 Emby 播放事件共用 `playback_sessions` 和 `user_item_state`。
- `PUT /api/v1/items/{itemId}/favorite`：按请求体 `{ "favorite": true }` 设置当前 Web 用户的收藏状态。
- `POST|DELETE /Users/{userId}/PlayedItems/{itemId}`、`/FavoriteItems/{itemId}`：幂等设置/清除已看和收藏状态。

本地媒体流支持完整响应和单 `Range: bytes=...` 请求，返回 200、206 或 416，并包含 `Accept-Ranges`、`Content-Length`、`Content-Range`、`Content-Type`、`ETag` 和 `Last-Modified`。媒体文件通过数据库 source ID 解析，读取前执行媒体库 ACL 和根目录路径安全检查。

字幕索引来自 ffprobe 内嵌轨和媒体文件同目录的同名外挂文件，支持 srt、ass、ssa、vtt、sub、sup；外挂字幕的语言、标题、forced 和 default 标记来自文件名，媒体流 DTO 会返回 `IsExternal`、`IsDefault` 和 `IsForced`。内嵌字幕不通过本阶段的读取端点抽取。

媒体 DTO 只返回客户端所需的标题、年份、简介、时长、容器、大小、码率和轨道信息，不返回服务器内部文件路径。图片内容端点属于 LUX-035。

媒体探测对本地文件使用 ffprobe；`.strm` 源优先读取同名 `-mediainfo.json` 和 NFO 的 `<fileinfo><streamdetails>`。管理员显式创建 STRM 探测任务后，宿主才会按 URL 安全策略调用 `org.lux.strm-media-info`，成功结果写入媒体源/媒体流并可选写回兼容旁车。PlaybackInfo 请求本身不访问外部地址；HTTP `.strm` 的播放入口只为解析重定向访问上游响应头，媒体内容仍由客户端按最终地址直连。旁车内容和插件结果只接受受限字段，不写入原始 ffprobe JSON、完整 URL 或凭据。

## 媒体库 ACL（LUX-036）

普通用户默认不能查看任何媒体库；管理员通过上面的管理接口授予 `canView` 后，Lux 和 Emby 的 Views、Items、详情及图片端点统一使用同一授权结果。无权库列表返回 403，已知无权条目或图片 ID 按 404 处理以避免 ID 探测。

## Emby 连接探针（LUX-023）

以下端点同时接受根路径和 `/emby` 前缀：

- `GET /System/Info/Public`
- `GET /System/Info`
- `GET|POST /System/Ping`

响应只返回 Lux 名称、版本、持久 ServerId 和必要能力字段，不返回配置目录、数据库路径或其他内部路径。LUX-023 的自动化测试是本地协议 shape 测试；VidHub、SenPlayer 和 Infuse 的真实连接证据要到 LUX-025 记录。
