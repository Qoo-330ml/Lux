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
- 远程请求还需命中 `LUX_TRUSTED_PROXY_CIDRS` 规则（若经反代）并满足用户 `can_remote_access`。
- 登录失败按来源和用户名限流；失败响应不区分用户不存在、密码错误或暂时封锁。
- `GET /api/v1/auth/me`：读取当前 Web session，返回用户和权限。
- `POST /api/v1/auth/logout`：需要有效 `lux_session` 和 `X-CSRF-Token`，成功返回 204 并撤销 session。

`lux_session` 为 `HttpOnly; Secure; SameSite=Lax; Path=/`，数据库只保存其 SHA-256 哈希。`lux_csrf` 不设置 HttpOnly，供同源 Web 客户端读取并通过 `X-CSRF-Token` header 发送；数据库保存 CSRF 哈希。session 有效期为 30 天，注销后立即失效。

当前阶段的 cookie 始终标记 `Secure`，部署时应使用 HTTPS；本机 HTTP 集成测试只验证协议和服务端行为，不代表浏览器会在不安全来源发送 Secure cookie。

## 媒体库管理（LUX-030）

以下接口要求有效 Web session；写操作还要求 `X-CSRF-Token`，并检查当前用户的 `canManageServer` 权限：

- `GET /api/v1/admin/libraries`：列出媒体库及其根路径。
- `POST /api/v1/admin/libraries`：创建媒体库。请求体为 `{ "name": "Movies", "kind": "MOVIE", "realtimeWatchEnabled": false, "scraperId": "tmdb" }`，`kind` 支持 `MOVIE`、`SERIES`、`MIXED`；`scraperId` 可省略或为 `null`，表示只使用本地元数据。
- `PATCH /api/v1/admin/libraries/{libraryId}`：运行时更新实时监听开关、增量/调和/元数据计划、扫描/探测并发、`scraperId` 和媒体库策略覆盖。字段均可省略；计划、`scraperId` 和 `mediaStrategy` 使用 `null` 清空，非空字符串最长 128 个字符；并发范围为 1-64。`mediaStrategy` 的结构与全局策略相同，未设置时返回 `null` 并继承全局默认。例如 `{ "scraperId": "tmdb", "metadataSchedule": "interval:5m" }`。修改无需重启，下一次任务读取最新配置；刮削器必须已安装且配置完成。
- `POST /api/v1/admin/libraries/{libraryId}/roots`：添加根路径。请求体为 `{ "path": "/media/movies" }`。
- `PATCH /api/v1/admin/users/{userId}/libraries/{libraryId}`：授予或撤销普通用户访问媒体库。请求体为 `{ "canView": true }`，需要管理员 Web session 和 CSRF。
- `POST /api/v1/admin/libraries/{libraryId}/scan`：创建并异步执行分批扫描任务，返回 202 和 job 状态。
- `POST /api/v1/admin/libraries/{libraryId}/reconcile`：按当前库配置创建并异步执行一次调和扫描；已停用或不存在的媒体库返回 404。
- `POST /api/v1/admin/jobs/{jobId}/cancel`：请求取消扫描任务，返回 202。
- `GET /api/v1/admin/jobs?page=1&pageSize=50&status=FAILED`：管理员分页查看扫描任务，可按 `PENDING`、`RUNNING`、`COMPLETED`、`CANCELLED` 或 `FAILED` 过滤。
- `GET /api/v1/admin/jobs/{jobId}/events?page=1&pageSize=100&level=ERROR&eventCode=SCAN_IO`：查看单个任务的结构化生命周期日志，支持级别和稳定事件代码筛选；页大小限制为 1-100。
- `POST /api/v1/admin/jobs/{jobId}/retry`：重试已失败或已取消的扫描任务，创建新的扫描任务并返回 202。
- `GET/PATCH /api/v1/admin/settings`：读取或调整 `resumePlayedPercent`（1-100）、`resumeMinTicks`（非负）和 `mediaStrategy`。媒体策略的图像开关为 `poster`、`logo`、`thumbnail`、`banner`、`disc`、`artwork`、`wallpaper`，另有元数据/图片语言、地区、默认刮削器、最大背景图数量、最小下载宽度、字幕默认值和 `applyScope`（`NEW_CONTENT`、`SELECTED_CONTENT`、`ALL_CONTENT`）。旧策略 JSON 缺少新增开关时按 `false` 处理；写操作需要管理员 Web session 和 CSRF，响应不包含任何插件凭据。
- `GET /api/v1/admin/health`：返回管理员可见的运行诊断，包括 schema、SQLite WAL 与实际写探针结果（`database.status`、`database.writable`）、配置目录实际写入能力、ffprobe、TMDb、媒体库根路径和后台任务计数；不返回本地路径或密钥。写入能力失败时整体 `status` 为 `degraded`，但仍返回可诊断的安全状态。
- `GET /api/v1/admin/logs`：返回脱敏的管理员审计事件，支持 `page`、`pageSize`、`level` 和 `eventCode` 筛选。

## 插件与刮削器（LUX-142）

以下接口要求 `canManageServer`；写操作还要求 `X-CSRF-Token`：

- `GET /api/v1/admin/plugins?page=1&pageSize=50`：分页返回启动时从 `/config/plugins` 发现的插件包及 `installed`、`enabled`、`running`、`configured`、`available`、`configurable`、`configFields`、`configSource`、`version`、`runtime`、`capabilities`、`status` 和脱敏 `lastError` 状态。`configFields` 只包含非敏感 schema，不包含当前值。
- `POST /api/v1/admin/plugins/{pluginId}/install`：启用已发现的插件包；它不会从网络下载代码。首次启用返回 201，重复请求返回 200。未知插件返回 404。
- `PUT /api/v1/admin/plugins/{pluginId}/config`：替换插件配置。TMDb 请求体为 `{ "apiKey": "..." }`；空字符串清除自定义 Key 并恢复内置 fallback。成功返回不含明文凭据的插件状态。
- 插件包必须是 `.zip` 或开发用解压目录，根目录包含 `manifest.json`。Lux 启动时校验包格式、协议版本、平台架构、文件哈希和签名；校验失败的包不会运行。
- 插件通过独立进程和 JSON-RPC 风格协议提供 `plugin.hello`、`plugin.health`、`metadata.search`、`metadata.get`、`metadata.images`、`metadata.externalIds`、`metadata.trailers` 和 `plugin.shutdown`。
- 未安装、未启用、无可用凭据、运行失败或未知的插件不能作为媒体库的 `scraperId`；选择不可用插件返回 `PLUGIN_UNAVAILABLE`。

插件包不从任意远程 URL 自动下载。插件 API、媒体库 API 和日志不返回插件配置中的敏感值；TMDb API Key 和 Read Access Token 只存在受限配置或内置实现中。

## 元数据候选管理（LUX-053）

- `GET /api/v1/admin/metadata/pending?page=1&pageSize=50`：管理员分页查看 pending 候选；页大小限制为 1-100。
- `GET /api/v1/admin/items/{itemId}/identify/candidates?q=关键词&page=1&pageSize=50`：管理员按 provider ID 或候选 JSON 搜索指定条目的 pending 候选，并返回 `fieldDiffs` 预览。
- `POST /api/v1/admin/items/{itemId}/identify/candidates`：管理员发送 `{ "query": "标题", "year": 2020 }` 搜索 TMDb；最多写入 20 个带 24 小时过期时间的 pending 候选，并返回当前条目的候选页。需要 `X-CSRF-Token`；TMDb 无可用凭据时返回服务不可用，TMDb 请求失败不会改变本地条目。
- `POST /api/v1/admin/items/{itemId}/identify/candidates/{candidateId}/select`：管理员选择候选并发送 `{ "mode": "fillMissing" }` 或 `{ "mode": "refreshUnlocked" }`，需要 `X-CSRF-Token`。前者只补空元数据字段和缺失图片，后者刷新未锁定字段和图片；候选中的每类图片只使用第一张，所属媒体库未启用的类型不写回，找不到的类型跳过；NFO/图片写回全部成功后才返回 `ONLINE_CONFIRMED`，失败返回可重试错误且候选保持 pending。
- `POST /api/v1/admin/metadata/reidentify`：管理员发送 `{ "itemIds": ["..."] }` 创建批量重新识别任务；条目去重后限制为 1-100 个，任务持久化为 `QUEUED/RUNNING/COMPLETED/FAILED`，需要 `X-CSRF-Token`。
- `GET /api/v1/admin/metadata/reidentify/{jobId}`：管理员读取批量重新识别任务及逐条状态、候选数量和稳定错误代码；前端可按任务 ID 轮询。
- `POST /api/v1/admin/metadata/reidentify/{jobId}`：管理员对 `FAILED` 任务重新排队失败条目，保留已经成功的条目，需要 `X-CSRF-Token`；非失败任务返回冲突。

根路径会先 canonicalize，再检查目录存在且可读；`isWritable` 独立返回。只读目录可以保存，但返回 `LIBRARY_PATH_NOT_WRITABLE` 警告。同一库的重复/重叠路径分别返回冲突/不可处理实体错误，跨库重叠返回结构化警告。

## Emby 认证（LUX-024）

- `GET /Users/Public`：返回未禁用用户的公开登录信息。
- `POST /Users/AuthenticateByName`：读取 `Username`/`Pw`，解析 `Authorization: Emby Client=..., Device=..., DeviceId=..., Version=...`，返回 `AccessToken`、`User`、`SessionInfo` 和 `ServerId`。
- `POST /Sessions/Logout`：接受 `X-Emby-Token` 或 `api_key`，撤销对应 token，成功返回 204。
- `System/Info` 和 `System/Ping`：需要有效的 `X-Emby-Token` 或 `api_key`；`System/Info/Public` 不要求认证。

Emby access token 与 Web session 完全分离。access token 是高熵随机值，只在认证响应中返回；数据库只保存 SHA-256 哈希以及设备元数据。认证失败响应不区分“用户不存在”和“密码错误”。

## 当前边界

`GET /health/ready` 在数据库可读但事务写入探针失败时返回 503 和 `reason=database_write_unavailable`；`/api/v1` 的写入接口统一返回 `DATABASE_UNAVAILABLE` 错误契约并包含 requestId。

上述接口是 LUX-021/LUX-022 的基础能力。媒体库、Emby 兼容、用户管理和进度接口按开发规格后续任务逐项增加；未实现端点不应被客户端兼容性声明引用。

## 电影查询（LUX-034）

Lux 电影查询要求有效 Web session：

- `GET /api/v1/libraries`：返回已启用媒体库的基本信息，不暴露服务器路径。
- `GET /api/v1/libraries/{libraryId}/items?page=1&pageSize=50`：按稳定标题顺序分页返回条目；支持 `itemType`、`year`、`isPlayed`、`isFavorite`、`sortBy=DateCreated` 和 `sortOrder=Ascending|Descending`，筛选和分页在 SQLite 查询中完成。
- `GET /api/v1/favorites?page=1&pageSize=50`：返回当前用户跨可见媒体库的收藏条目，按最近添加倒序分页；服务端执行用户状态和媒体库 ACL。
- `GET /api/v1/search?q=关键词&page=1&pageSize=50`：搜索标题、原标题和别名，结果执行媒体库 ACL。
- `GET /api/v1/home`：返回当前用户继续观看、推荐和可见媒体库入口；每个媒体库入口包含最多 12 条该库最新资源，按 `media_items.added_at` 倒序。所有内容均执行媒体库 ACL；响应中的 `recentlyAdded` 字段保留用于旧客户端兼容，Lux Web 首页按媒体库分别展示最新资源。
- `GET /api/v1/items/{itemId}/playback`：读取当前 Web 用户的播放位置、已看和收藏状态。
- `POST /api/v1/items/{itemId}/progress`：写入播放进度，需要当前 Web session 和 CSRF。
- `PUT /api/v1/items/{itemId}/favorite`：设置当前 Web 用户的收藏状态，需要当前 Web session 和 CSRF。
- `PUT /api/v1/items/{itemId}/played`：设置当前 Web 用户的已看状态，请求体为 `{ "played": true }`，需要当前 Web session 和 CSRF。
- `GET /api/v1/items/{itemId}`：返回电影详情、媒体源和已探测轨道。
- `GET /api/v1/items/{itemId}/children?itemType=SEASON|EPISODE&seasonId=...`：Web 同源读取剧集季度/单集或合集成员，结果执行当前用户 ACL。
- `GET /api/v1/collections/{collectionId}`：返回可访问 BOX_SET 及按媒体库 ACL 过滤后的成员。
- `GET|POST /api/v1/admin/users`、`PATCH|DELETE /api/v1/admin/users/{userId}`：管理员管理用户、权限和禁用状态；删除为禁用语义，最后一个服务器管理账户受保护。
- `GET /api/v1/admin/users/{userId}/libraries`：读取该用户当前可访问的媒体库 ID，用于管理控制台展示 ACL；不返回服务器路径。
- `GET /api/v1/admin/audit?page=1&pageSize=50`：管理员分页读取管理操作审计事件。
- `GET /api/v1/admin/jobs/{jobId}`：管理员读取单个扫描任务详情，包括状态、进度、游标和错误。
- `GET /api/v1/admin/items/{itemId}/images`、`DELETE /api/v1/admin/items/{itemId}/images/{imageId}`：管理员查看图片索引并删除媒体根目录内的图片及索引；删除要求 CSRF，响应不暴露本地路径。
- `DELETE /api/v1/admin/items/{itemId}`：管理员删除指定媒体源及其同名旁车文件；若媒体文件已被外部删除，仍会清理 Lux 中的媒体源记录，没有其他媒体源时同时标记逻辑条目移除。支持通过 `sourceId` 选择版本，要求 CSRF。
- `GET /api/v1/auth/sessions`、`DELETE /api/v1/auth/sessions/{sessionId}`：当前用户查看并撤销其他 Web 会话；删除要求 CSRF，当前会话必须通过退出登录撤销。
- `GET|HEAD /api/v1/items/{itemId}/images/{type}`、`/{type}/{index}`：读取本地 poster/fanart，支持 ETag 和 `If-None-Match`。

Emby 电影查询要求有效 `X-Emby-Token` 或 `api_key`：

- `GET /Users/{userId}/Views`：返回电影媒体库视图。
- `GET /Users/{userId}/Items`、`GET /Items`：支持 `ParentId`、`StartIndex`、`Limit` 和 `IncludeItemTypes=Movie`，默认从 0 开始、每页 50 条，单页上限 100。
- `GET /Users/{userId}/Items`、`GET /Items`：另支持 `IsPlayed`、`IsFavorite`、`Years`、`SortBy` 和 `SortOrder`，筛选后再分页。
- `GET /Users/{userId}/Items/{itemId}`、`GET /Items/{itemId}`：返回 Emby 兼容电影详情 DTO。
- `GET /Shows/{seriesId}/Seasons`：按用户媒体库权限返回季度。
- `GET /Shows/{seriesId}/Episodes?SeasonId={seasonId}&StartIndex=0&Limit=50`：返回剧集，可省略 `SeasonId` 获取整部剧集，支持分页。
- `GET /Users/{userId}/Items/NextUp`：按该用户的播放状态返回未看完单集。
- `GET|HEAD /Items/{itemId}/Images/{Type}`、`/{Type}/{Index}`：读取与 Lux API 相同的本地图片记录，支持 `X-Emby-Token` 或 `api_key`。
- `GET /Users/{userId}/Items/Resume`：按用户播放位置、已看状态和服务器 Resume 阈值返回继续观看列表。
- `GET /Users/{userId}/Items/Latest`：按最近添加顺序返回当前用户可见媒体。
- `GET /Search/Hints?SearchTerm=关键词&StartIndex=0&Limit=50`：返回 Emby 搜索提示，结果执行当前用户 ACL。
- `GET|HEAD /api/v1/items/{itemId}/subtitles/{streamIndex}`：读取指定外挂字幕流；需要 Web session，并执行媒体库 ACL。
- `GET|HEAD /api/v1/items/{itemId}/stream`：读取默认本地媒体源；可通过 `sourceId` 选择媒体源，需要 Web session 和媒体库 ACL。
- `GET|HEAD /Videos/{itemId}/{mediaSourceId}/Subtitles/{streamIndex}/Stream`：按指定媒体源读取外挂字幕。
- `GET|HEAD /Items/{itemId}/Subtitles/{streamIndex}/Stream`：按条目读取默认媒体源的外挂字幕。
- `GET|HEAD /Videos/{itemId}/stream`、`/Videos/{itemId}/stream.{container}`：读取默认本地媒体源。
- `GET|HEAD /Videos/{itemId}/{mediaSourceId}/stream`、`/stream.{container}`：读取指定本地媒体源。
- `GET|HEAD /Items/{itemId}/Download`：需要 `can_download` 和媒体库 ACL，返回附件下载流。
- `GET|HEAD /api/v1/items/{itemId}/download`：Lux 下载端点，需要 Web session、`can_download` 和媒体库 ACL。
- `GET|POST /Items/{itemId}/PlaybackInfo`：返回可访问媒体源、媒体流和 DirectPlay 能力；支持 `MediaSourceId` 显式选择，当前不声明转码或 DirectStream。每个媒体源可带 `Edition`/`Quality` 版本标签。
- `MediaSources.Path` 对 `.strm` 源返回旁车记录中的外部媒体地址；`MediaStreams` 除基础轨道字段外，还返回旁车中的分辨率、画面比例、码率、色深、帧率、Profile、像素格式、声道布局和采样率等已验证字段。
- `GET /Items/{collectionId}/Children`：返回按当前用户媒体库权限过滤的合集成员。

`.strm` 媒体源在 PlaybackInfo 中以 `Protocol=Http`、`IsRemote=true` 和原始 `DirectStreamUrl` 返回；服务端不请求、不验证、不代理该 URL。具有媒体库访问权限的客户端会直接获得该地址，因此 URL 中的令牌也会按产品设计暴露给客户端。

- `GET /Sessions`：返回当前用户的活动播放会话；管理员可查看全部活动会话。
- `POST /Sessions/Playing`、`/Sessions/Playing/Progress`、`/Sessions/Playing/Stopped`：幂等记录播放事件，并将位置单调写入用户状态。
- `GET /api/v1/items/{itemId}/playback`：读取当前 Web 用户的播放状态。
- `POST /api/v1/items/{itemId}/progress`：写入当前 Web 用户的播放位置。
- `PUT /api/v1/items/{itemId}/favorite`：按请求体 `{ "favorite": true }` 设置当前 Web 用户的收藏状态。
- `POST|DELETE /Users/{userId}/PlayedItems/{itemId}`、`/FavoriteItems/{itemId}`：幂等设置/清除已看和收藏状态。

本地媒体流支持完整响应和单 `Range: bytes=...` 请求，返回 200、206 或 416，并包含 `Accept-Ranges`、`Content-Length`、`Content-Range`、`Content-Type`、`ETag` 和 `Last-Modified`。媒体文件通过数据库 source ID 解析，读取前执行媒体库 ACL 和根目录路径安全检查。

字幕索引来自 ffprobe 内嵌轨和媒体文件同目录的同名外挂文件，支持 srt、ass、ssa、vtt、sub、sup；外挂字幕的语言、标题、forced 和 default 标记来自文件名，媒体流 DTO 会返回 `IsExternal`、`IsDefault` 和 `IsForced`。内嵌字幕不通过本阶段的读取端点抽取。

媒体 DTO 只返回客户端所需的标题、年份、简介、时长、容器、大小、码率和轨道信息，不返回服务器内部文件路径。图片内容端点属于 LUX-035。

媒体探测仅对本地文件优先使用 ffprobe；`.strm` 源不主动读取外部媒体的容器信息或索引，只优先读取同名 `-mediainfo.json`，再读取同名 NFO 的 `<fileinfo><streamdetails>`。没有有效旁车时技术信息保持为空，首次播放由客户端直接访问外部地址。旁车内容只接受受限字段，不执行外部地址探测。

## 媒体库 ACL（LUX-036）

普通用户默认不能查看任何媒体库；管理员通过上面的管理接口授予 `canView` 后，Lux 和 Emby 的 Views、Items、详情及图片端点统一使用同一授权结果。无权库列表返回 403，已知无权条目或图片 ID 按 404 处理以避免 ID 探测。

## Emby 连接探针（LUX-023）

以下端点同时接受根路径和 `/emby` 前缀：

- `GET /System/Info/Public`
- `GET /System/Info`
- `GET|POST /System/Ping`

响应只返回 Lux 名称、版本、持久 ServerId 和必要能力字段，不返回配置目录、数据库路径或其他内部路径。LUX-023 的自动化测试是本地协议 shape 测试；VidHub、SenPlayer 和 Infuse 的真实连接证据要到 LUX-025 记录。
