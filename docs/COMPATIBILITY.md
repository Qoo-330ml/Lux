# Lux 客户端兼容性矩阵

本文档是目标客户端兼容性的唯一事实来源。未填入实测版本和证据前，不得宣称兼容。

## 目标矩阵

| 客户端 | 版本 | 平台/设备 | 添加服务器 | 登录 | 浏览/详情 | 播放 | 进度/收藏 | 字幕/多版本 | 证据/备注 |
|---|---|---|---|---|---|---|---|---|---|
| Infuse | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 未测试 | 待 LUX-025 |
| VidHub | 2.1.8 | macOS arm64 | 通过 | 通过 | 媒体库浏览、条目详情通过 | 通过 | 通过 | 未测试 | 2026-08-05 本机 ARM64 真实 UI 播放本地 MKV，Playing/Progress/Stopped 回传和 Resume 读回通过；收藏/已观看状态另有 2026-08-03 证据 |
| SenPlayer | 6.0.6 | macOS arm64 | 通过 | 通过 | 首页、电影列表通过 | 通过 | 未测试 | 未测试 | 2026-08-07 本机 ARM64 真实 UI 播放 `.strm` 电影通过；服务端兼容客户端生成的小写 `/emby/videos` 和路径内编码查询参数，并对远程源返回 307 直连重定向 |
| Harbor | 1.4.6 | macOS arm64 | 通过 | 通过 | 媒体库浏览、条目列表通过 | 未测试 | 未测试 | 未测试 | 2026-08-09 本机 Harbor 连接本机 Lux 后，媒体库详情请求 `/Users/:userId/Items/:libraryId` 从 404 修复为 200，并进入电影库显示条目 |
| Lux Web | Chrome 150 smoke | macOS arm64 | 通过 | 通过 | 基础浏览/详情/筛选/账户会话通过 | MP4 直放通过 | 进度/收藏接口与收藏浏览器 smoke 通过 | 多版本代码已实现、字幕路径已有服务端测试 | Chrome headless：普通用户无管理入口、stream 206、readyState=4、390/768/1440 viewport 无横向溢出、控制台无错误；`scripts/browser-smoke.mjs` 和 `scripts/admin-smoke.mjs` 已固化 |

## Lux Web 4K 媒体能力探针

LUX-184 已加入独立探针页面 `/media-capability-probe.html`，用于记录真实媒体样本在原生 `video`、
MediaCapabilities 和 WebCodecs 下的能力。探针能力声明和 LUX-185 客户端 fallback 性能记录分开维护；
fallback 的 4K 实时能力不因探针返回 `supported` 而自动宣称。

2026-08-17 的本机探针记录：Playwright HeadlessChrome 151、macOS `arm64` 对 4K HEVC Main、HEVC Main10/HDR10
和 H.264 的 3840×2160 配置均报告原生 `probably`、MediaCapabilities `supported/smooth/powerEfficient` 和
WebCodecs `supported`。这只是浏览器能力声明，不是实际 4K 文件播放证据。

同次使用公开 Sintel 片段生成的临时 12 秒 HEVC MP4 做解码链路烟测，浏览器实际识别为 854×480，metadata、
播放位置和 5 秒播放均正常；该文件不是 4K 样本，不能用于 4K 性能结论。

## Lux Web LUX-185 客户端 fallback 实测

2026-08-17，提交 `1233ec95`（包含性能状态提交 `fa39190a`），Playwright HeadlessChrome 151.0.0.0，macOS
`arm64`（`uname -m=arm64`）。
样本均为临时、本地、无个人数据文件，完整 URL 不写入记录：

| 样本 | SHA-256 | 结果 | 性能与质量 |
|---|---|---|---|
| 3840×2160 HEVC Main 8-bit + AAC、MP4、8 秒、9.4 MiB | `cbfad82624c6578ea9ce5f2a0f5e229d0230745d7cfa84eb9b5d457b57920ce1` | Worker/WASM 解码、H.264 编码、视频/音频缓冲、播放、暂停、seek、destroy 通过 | Worker 累计处理 21,558.7 ms，媒体时长 8,000 ms，`speedX=0.371`，低于实时；播放 2 秒窗口 50 帧/0 丢帧，漂移约 30 ms；seek 到 4 秒后漂移约 36 ms |
| 3840×2160 HEVC Main10 10-bit HDR10、MP4、约 4.13 秒、21 MiB，无音频 | `88b238b05eca4de87548f5d2b022ddf1daa2e60d4f0218e65ae04db770d1d2da` | WASM 解码、H.264 编码、播放、seek、destroy 通过 | Worker 累计处理 18,929.3 ms，媒体时长 4,086 ms，`speedX=0.216`，低于实时；播放窗口 24 帧/0 丢帧；seek 通过。原始样本音轨为 DTS，测试文件主动去除音频，不代表 AAC 兼容 |

`43a7b8e6` 的流式增量复测确认：`setSource()` 在完整转码完成前即可返回。HEVC Main 在 4,537 ms 返回首段并在
17,665 ms 完成全片，HEVC Main10 在 9,606 ms 返回首段并在 18,577 ms 完成全片；两者首段均收到首帧，seek
误差为 0 ms，`requestVideoFrameCallback` 的 `presentedFrames` 序列没有 gap，Main + AAC 的播放末段音画差约
44 ms。HeadlessChrome 的 `getVideoPlaybackQuality().droppedVideoFrames` 在该测试中与 presented-frame 序列不一致，
因此以 presented-frame gap 作为丢帧判断，并保留该 API 差异作为测试注意事项。

`79035ba7` 另以真实 `PlayerPage` 集成路径复测 4K Main：在新鲜 HeadlessChrome 151 中模拟原生 HEVC 不可用，
浏览器实际选择客户端 fallback，首帧约 11,493 ms，页面显示 `speedX≈0.37` 的低于实时提示；播放和暂停分别
上报 `PLAYING`/`PAUSED` 进度，浏览器控制台无播放相关错误。4K 选择器使用与 `@hevcjs/core` 一致的 H.264
High@5.1 (`avc1.640033`) 探测，避免 4K 错误使用 High@4.0 (`avc1.640028`) 而被 WebCodecs 拒绝。

因此当前这台 ARM64/HeadlessChrome 设备可以完成 4K HEVC 客户端 fallback，但 4K Main 和 Main10 均未通过实时
转码性能门。播放器会显示“客户端解码速度低于实时”的降级提示，并建议使用原生客户端或降低清晰度；不能把
本次结果外推到其他浏览器、硬件或目标 x86_64 NAS。

记录探针结果时必须包含浏览器版本、平台/设备、Lux 提交、`uname -m`、样本校验值、metadata 结果、实际
播放时长、VideoFrame 数量、丢帧和音画同步观察。不得写入完整媒体 URL、令牌、Cookie 或用户数据。

## 记录格式

每次探针或回归测试至少记录：客户端版本、平台版本、Lux 提交、请求路径序列、脱敏请求参数、状态码、关键响应字段、结果和已知差异。密码、token、Cookie、真实 `.strm` URL 和用户数据不得进入 fixture 或文档。

## 当前状态

### Lux 原生出站 Webhook

Lux 当前提供一个版本化的原生 Webhook 合同（`schemaVersion: 1`），用于发送媒体、扫描、元数据、后台任务
和播放事件。请求使用 `X-Lux-Event-Id`、时间戳和 HMAC-SHA256 签名，投递为至少一次语义，接收方应按
`eventId` 幂等。Webhook 目标可以选择 Lux 原生或 Emby 风格的有限 DTO payload；两者均经过字段白名单和脱敏
处理。该功能不是 Emby Webhooks 插件的完整兼容实现，不支持未列入测试合同的模板变量、插件事件或行为。

当前只提供 Webhook 渠道；Telegram、企业微信和 Email 尚未实现。

- 媒体库实时监听默认开启。复制到已配置根路径中的新视频会进入局部 `INCREMENTAL_SCAN`，只处理该事件路径，通常在几秒内进入索引；旧版 `realtimeWatchEnabled` 请求字段不会关闭监听。
- LUX-000 至 LUX-003：仅完成仓库工程检查，尚未连接任何真实客户端。
- LUX-023：已完成根路径/`/emby` 前缀的 System/Ping 本地协议 shape 测试；`GET/POST /System/Ping` 按 Emby OpenAPI 兼容为无需认证的空 200，并完成 VidHub/SenPlayer 真实登录前置探针。
- LUX-024：已完成 Users/Public、AuthenticateByName、Sessions/Logout 的本地协议 shape 和 token 脱敏测试；VidHub 真实登录通过；SenPlayer 认证响应解析失败的历史缺口已补充更完整的 `User`/`SessionInfo` shape，并补齐认证后 `GET /Users/:userId` 用户详情路由；P0 真实 UI 复测已通过。
- Emby `GET /Items/Counts` 现已支持根路径和 `/emby` 前缀，执行 Emby token/API key 鉴权、用户媒体库 ACL，并支持 `UserId` 与 `IsFavorite` 过滤；`tests/emby_counts.rs` 覆盖协议响应。尚未以 Filmly 或 CapyPlayer 真实 UI 复测，不据此宣称客户端兼容。
- 2026-08-11 服务器名称兼容修复：第三方客户端添加服务器时可从 `GET /System/Info/Public` 的 `ServerName` 读取名称；认证后的 `GET /System/Info`、`Users/Public`、`AuthenticateByName` 返回的 `User.ServerName` 也统一读取管理员设置的 `serverName`。官方 Emby 文档将 `ServerName` 定义为服务器名称字段；本次加入 `tests/emby_system.rs` 协议回归，尚未在当前环境重新进行 VidHub UI 点选复测。
- SenPlayer 列表兼容修复：当请求的 `Fields` 未包含 `MediaSources` 或 `MediaStreams` 时，Emby 列表响应不再携带这些字段；详情和 `PlaybackInfo` 仍返回完整媒体源。自动化回归已覆盖，真实客户端需要清理缓存或重新进入库后复测。
- 2026-08-17 SenPlayer `.strm` 直放地址编码修复：HTTP(S) 目标包含 Unicode 路径或查询参数时，Emby 视频端点现在先将 URL 规范化为合法的百分号编码 `Location`，再返回原有 307 直连；数据库仍保留原始目标，不代理媒体字节。新增 API 单测和 `.strm` 集成回归，真实 SenPlayer UI 需重新部署后复测。
- Emby `GET /Items` 对标准 ItemId 仍按逗号分隔的 `Ids` 严格过滤；不存在的 ItemId 或 UUID 返回空 `Items` 和 `TotalRecordCount: 0`。针对 Redia 的兼容兜底见下一条。
- Redia 兼容兜底：`GET /Items?Ids=<MediaSourceId>` 在没有同名 ItemId 时会解析到该媒体源所属条目；未知 ID 仍返回空结果，不会回退到媒体库第一条。`/Videos/{ItemId}/original.strm`（含 `/emby` 和大小写路径变体）复用 Emby 播放逻辑并对 STRM 返回 307 直连；其他未注册 `/Videos/...` 路径返回 404，不再落入 Web 前端 fallback 返回 HTML。标准客户端仍应使用 ItemId 和 `/Items/{ItemId}/PlaybackInfo`。
- `cargo` 验证是在本机 `arm64` 上完成，不代表目标 x86_64 飞牛 NAS 性能或客户端兼容性。
- Web 的“已实现”仅表示代码路径和服务端静态集成已完成；当前 Chrome smoke 覆盖登录、筛选、播放、收藏、账户会话和管理流程，不等同于所有浏览器/编码格式兼容。
- LUX-121 兼容补齐：Emby `Views` 返回媒体库类型、`ChildCount` 和标准 `ImageTags.Primary`；条目详情同时返回本地徽标的 `ImageTags.Logo`，并通过 `/Items/{itemId}/Images/Logo` 提供标准图片读取；媒体库封面支持 `/Items/{libraryId}/Images/Primary` 及带索引、HEAD、ETag 和 ACL。尚待 VidHub UI 重新实测确认。
- 2026-08-17 混合媒体库兼容修复：普通 Emby 客户端继续在混合库 DTO 中收到 `CollectionType: "mixed"`；VidHub 的认证设备收到历史兼容的 `CollectionType: null`，同时保留电影/剧集条目列表和 `TypeOptions`。原因是 VidHub 可连接服务器但会丢弃带 `mixed` 集合类型的库；`tests/mixed_library_api.rs` 覆盖两种客户端的分流。
- Emby `GET /Library/VirtualFolders` 现返回接近官方 `VirtualFolderInfo` 的完整结构：`Id`、`Guid`、`ItemId` 使用同一个稳定媒体库 ID，`LibraryOptions` 包含 `PathInfos`、按电影/剧集类型拆分的 `TypeOptions`、Lux 当前图片策略、NFO 本地元数据策略、字幕语言和播放恢复阈值。Lux 没有等价 Emby 刮削器时，metadata/image fetcher 数组保持为空；尚未以目标第三方客户端真实 UI 复测该管理端点。
- Emby `GET /Persons` 已支持 `Recursive`、`Fields`、`SortBy=Name|DateCreated`、`SortOrder` 和任意正整数 `Limit`；返回去重后的演员 `Items`、`TotalRecordCount` 及 `DateCreated`，顶层结构与 Emby 一致且不额外返回 `StartIndex`。人物 DTO 使用 Emby 的 `Type: "Person"`，并补齐 `ServerId`、`ImageTags` 和 `BackdropImageTags`。支持根路径和 `/emby` 前缀、Emby token/API Key、媒体库 ACL，并在服务启动后台回填已有 `people.json` 关系到人物索引。`tests/people_api.rs` 已覆盖共享 API Key、跨条目聚合、字段投影、排序、超大 Limit、前缀和回填；尚未以目标第三方客户端真实 UI 复测。
- Emby `GET /Persons/{personIdOrName}` 已与人物列表 DTO 对齐，优先按人物 ID、未匹配时按精确姓名查询，并支持 `/emby` 前缀、`Fields`、共享 API Key、用户媒体库 ACL 和人物图片标签；为兼容 MDC，`GET/HEAD /Items/{personId}` 也返回相同人物 DTO，`POST /Items/{personId}` 接收 MDC 提交的演员元数据并更新可访问媒体库中的人物关系后返回相同 DTO，`POST /Items/{personId}/Images/Primary` 接收 MDC 的 Base64 演员头像（也兼容 Emby 原始图片二进制，并按解码后的实际 JPEG/PNG/WebP 签名识别格式，即使 Content-Type 声明不准确）并返回 `204 No Content`，已鉴权的无 tag `Primary` 人物图片请求同样受支持。Lux Web 提供受 session/API Key 保护的 `GET /api/v1/people/{personId}` 人物详情，供人物详情页读取。人物详情不因缺少简介或头像而返回空响应；MDC 元数据和头像 POST 兼容已由 `tests/people_api.rs` 覆盖，尚未以目标第三方客户端真实演员刮削流程复测。
- Harbor 1.4.6 兼容修复：Emby 媒体库自身的 `/Users/{userId}/Items/{libraryId}` 详情现在返回 `CollectionFolder`，并复用媒体库启用状态和 ACL 校验；本机 Harbor 真实 UI 已验证可进入库并显示条目。
- 2026-08-10 Emby 目录兼容修复：`Items/Latest` 默认按 `GroupItems=true` 返回电影/剧集根条目，剧集与季度 DTO 补充 `ChildCount`/`RecursiveItemCount`；`ParentId` 现在支持媒体库、剧集和季度，并覆盖剧集单集查询。`tests/series_api.rs` 已加入协议回归覆盖；网易爆米花真实设备复测仍待完成。
- 2026-08-11 网易爆米花 2.15.3 DTO 兼容修复：已观察到客户端可登录并加载部分首页，但尚未进入播放会话。Emby 条目现补齐 `SortName`、`SeasonId`、`IndexNumber`、`PremiereDate` 和 `ProviderIds`，季/集层级的标准字段已有协议回归覆盖；完整首页、详情页和播放仍待重启服务后的真实设备复测，不据此宣称完全兼容。
- 2026-08-11 网易爆米花首页链路修复：补齐 Emby 用户虚拟根目录 `/Users/{userId}/Items/Root`、`/Items/Root?userId=...` 和 `CollectionFolder` 子项，修正媒体库范围 `Items/Latest` 将季/集误当成最新根条目的问题；协议回归已覆盖，真实设备首页复测仍待完成。
- 2026-08-14 网易爆米花 2.15.3 搜索兼容修复：客户端通过 Emby `/Users/{userId}/Items?SearchTerm=...` 搜索时，服务端此前忽略 `SearchTerm` 并返回未过滤目录，导致搜索页显示无关条目；现已接入标题、原始标题和别名搜索并执行 ACL。重启本机 ARM64 服务后，爆米花 macOS UI 搜索“鬼吹灯之南海归墟”返回匹配条目，不再返回未过滤目录。
- 2026-08-12 网易爆米花媒体库列表 DTO 对齐：针对其实际请求的 `SortBy=DateCreated,SortName`、`Fields=BasicSyncInfo,ChildCount,RunTimeTicks,CommunityRating,PremiereDate,ProductionYear,CanDownload`，列表响应现在省略未请求字段和空值，返回 `SupportsSync`、UTC ISO `PremiereDate`、无播放位置时不返回 `PlayedPercentage`，并批量返回 `Logo`、`Thumb`、`Banner`、`Disc` 与全部背景图标签；电影 `ParentId` 由扫描建立的物理目录条目提供。脱敏协议回归已覆盖，网易爆米花真实设备刷新复测仍待完成，不据此宣称完全兼容。
- 2026-08-14 Filmly 2.12.3 剧集详情集列表 DTO 对齐：针对真实请求 `Fields=BasicSyncInfo,Overview,ProviderIds,Path,Size,People,RuntimeTicks,Chapters,MediaSources,CanDownload`，剧集单集在没有独立海报时将本地 `-thumb` 映射为 `ImageTags.Primary`，补齐 `SupportsSync`、Overview、ProviderIds、SeasonName、ParentThumbItemId、Size/Container/Bitrate、People 空集合，并在 `MediaSources` 内返回 `MediaStreams`。`tests/series_api.rs` 已覆盖脱敏请求和关键字段；真实安卓设备复测仍待完成，不据此宣称完全兼容。
- 2026-08-14 Filmly Episode 媒体流 shape 进一步对齐参考 Emby：集列表在请求媒体源时保留 `PremiereDate`，媒体源补齐 `SupportsProbing`，媒体流补齐 `AttachmentSize`、`IsAnamorphic`、`Protocol` 和 `SupportsExternalStream`，并由剧集协议回归锁定；远端设备仍需重新部署后复测。
- 2026-08-14 Filmly 图片兼容修复：实测 Android/Filmly 图片加载不携带 Emby token，且部分集只有 `THUMB` 图片却在 DTO 中作为 `ImageTags.Primary` 使用，导致 `/Items/{id}/Images/Primary` 返回 401/404。Emby 图片端点现在在 Primary 无 Poster 时回退到 Thumb，覆盖带 tag、无 tag 和已认证请求；`tests/series_api.rs` 已加入三种路径回归，真实设备需部署后复测。
- STRM 兜底图片兼容修复：没有刮削器、刮削器无候选或候选没有主图时，启用 STRM 截图会生成同一文件并同时登记为 `POSTER` 与 `THUMB`，因此 Emby `ImageTags.Primary`、`Thumb` 和 Lux Web 海报入口都能读取；相关数据库迁移、无刮削器集成回归和共享文件删除保护已覆盖，目标第三方客户端需部署后复测。
- 2026-08-14 Filmly 详情状态请求补齐：`Shows/NextUp` 现在按请求的 `SeriesId` 过滤，并遵守 `EnableTotalRecordCount=false` 的分页 shape；季度列表请求 `Genres` 时补齐 `Genres`/`GenreItems` 空集合，避免客户端收到错误剧集状态或不完整季度 DTO。
- 2026-08-14 Filmly Android 详情页根因定位：部分媒体源的 `MediaStreams[].Language` 为 JSON `null` 时，爆米花详情页显示“尝试连接时发生错误”；仅将剧集分集接口对 Filmly User-Agent 的空语言规范化为 `"und"` 后，失败资源恢复正常。VidHub/其他客户端及播放接口保持原始字段；`tests/series_api.rs` 已覆盖两种客户端响应差异。
- 2026-08-11 Filmly 2.12.3 首页请求修复：`/Users/{userId}/Items` 现在支持 `ExcludeItemTypes`，未指定递归和类型时按 Emby 根层级返回电影/剧集，列表 DTO 补充用户 `CanDownload` 和请求的 `Chapters` 字段；已用真实请求参数加入剧集层级协议回归，真实设备刷新复测仍待完成。
- 播放兼容修复：本地源的 Emby `Container` 使用真实文件扩展名，播放 URL 由 `MediaSourceId` 定位文件并兼容复合容器旧后缀；`attached_pic` 不再暴露为视频轨。自动化播放/探测回归已覆盖 MKV 和 MP4 路径，VidHub 已实测本地 MKV 直放。
- 播放会话失活保护：若第三方客户端异常退出、网络中断或未发送 `Stopped`，`PLAYING`/`PAUSED` 会话在连续 90 秒没有事件后从 Emby `GET /Sessions`、管理员控制台和 Web 播放状态中隐藏；显式 `Stopped` 仍立即清理活动会话。
- LUX-091 下载回归已覆盖 Lux/Emby 的 GET/HEAD 单资源响应、Range/文件名响应，以及 `.strm` 远程资源流式转发；尚未完成第三方客户端的真实下载 UI 实测，因此不据此宣称 Infuse、VidHub 或 SenPlayer 下载兼容。
- LUX-160 通用 `.strm` 解析回归已覆盖路径目标的 `PlaybackInfo` Lux 入口、受监督解析器 RPC 和 307 转发；尚未完成目标客户端现场配置与播放实测，不据此宣称客户端兼容。
- LUX-151 IP 归属地只扩展 Lux 管理员 Web 仪表盘的 `nowPlaying` 数据，不改变 VidHub、SenPlayer 或 Infuse 的 Emby 兼容接口；Hiofd 出站可用性和归属地准确性尚未做目标 NAS 现场验证。
- LUX-165 图片资源布局回归：Rust 集成测试已验证新下载图片写入 `/config/metadata/library/<shard>/<item-id>/`，Lux/Emby 图片端点可读取该路径，媒体目录本地图片仍可读取，且删除仅允许两类受保护根目录；这属于服务端协议回归，不替代 VidHub、SenPlayer 或 Infuse 的真实客户端复测。
- 本地 NFO 派生缓存回归：详情接口只读取数据库快照；快照损坏或过大时会清理该派生行并继续返回基础条目，演员关系或人物头像损坏时保留演员文字信息并由 Web 使用人物占位图标。该行为已由 `tests/catalog.rs`、`tests/metadata.rs` 和人物单元测试覆盖，尚未替代第三方客户端现场复测。
- LUX-166 元数据对象路径回归：Rust 路径契约测试已验证 `collections`、`genres`、`studios`、`tags` 的展示名桶、provider/object ID 身份和越界拒绝；本任务不改变客户端 API 行为。
- LUX-167 元数据对象快照回归：合集刷新协议测试已验证数据库关系更新后生成 `collection.json`，快照写入失败映射为可重试的服务错误；genres、studios、tags 尚无在线对象数据源，因此仅验证共用存储能力。

## LUX-025 本机探针进度（2026-08-02）

| 客户端 | 本机发现 | 已观察到的流程 | 当前结果 |
|---|---|---|---|
| VidHub 2.1.8 | 已安装并运行 | 已完成 Emby 添加服务器、登录并进入 Lux 空媒体库 | 添加服务器/登录通过；旧探针发生在 `Views/Resume` 实现前，当前服务端已有对应路径和自动化测试 |
| SenPlayer 6.0.6 | 已安装 | 修复后已完成服务器加载、认证、`Users/:userId`、Views、Resume 和 Items 请求；电影页已显示 16 个条目（服务端总数 22） | P0 连接/登录和电影列表浏览通过；详情、播放、进度、收藏、字幕和多版本尚未实测 |
| Infuse | 未发现已安装应用 | 无法开始本机 UI 探针 | 未测试，需安装后再测 |

本次 VidHub 探针使用临时本机 ARM 服务 `127.0.0.1:18099`，未记录密码、token、Cookie、用户 ID 或真实媒体数据。

## VidHub 最新 ARM64 实测（2026-08-03）

VidHub 2.1.8（macOS arm64）连接本机独立 ARM64 实例 `http://127.0.0.1:18612`，服务端镜像为 `lux:arm64-local`（revision `83b5977`），使用临时媒体库和有效 MP4 夹具。真实 UI 流程如下：

| 流程 | 结果 | 证据 |
|---|---|---|
| 添加服务器并登录 | 通过 | VidHub 显示 `Lux ARM64 Full Smoke Emby - http://127.0.0.1:18612` 并进入库首页 |
| 媒体库浏览 | 通过 | 显示 `VidHub Smoke Movies` 和 `VidHub Valid 2024` |
| 条目详情 | 通过 | 详情页显示标题、年份和播放入口 |
| 本地 MP4 直放 | 通过 | VidHub 播放器进入 `VidHub Valid` 播放页面；初始 10 字节伪 MKV 的失败提示属于无效测试夹具，换成有效 MP4 后播放成功 |
| 收藏/已观看 | 通过 | UI 开关操作后，Lux API 返回 `isFavorite=true`、`isPlayed=true`、`playCount=1` |
| 播放位置上报 | 未观察 | 30 秒 MP4 播放并退出后，服务端 `positionTicks` 仍为 0；不把服务端接口测试当作真实客户端进度证据 |

本次测试没有记录密码、token、Cookie 或真实媒体数据。字幕、多版本和 Infuse 仍未完成真实客户端实测。

VidHub 2.1.8 登录后请求序列（动态用户 ID 已脱敏；这是服务端实现 `Views/Resume` 前的历史探针）：

| 方法 | 路径 | 状态 | 结果 |
|---|---|---:|---|
| GET | `/emby/Users/:userId/Views` | 404 | 未实现的媒体库视图路径 |
| GET | `/emby/Users/:userId/Items/Resume` | 404 | 未实现的继续观看路径 |

这组 404 只代表当时运行的服务端版本，不代表当前源码状态。当前源码已提供这两条路径；`tests/acl.rs` 覆盖 `Views`，`tests/resume_favorites.rs` 覆盖 `Items/Resume`。上述最新 ARM64 实测已补充真实客户端浏览、详情、播放和用户状态证据。

## VidHub 播放进度回传实测（2026-08-05）

VidHub 2.1.8（macOS arm64）连接当前 Mac 地址 `http://192.168.50.108:8097`，使用包含回调字段兼容和 `PlaySessionId` 响应修复的工作树构建。此前保存的 `192.168.50.113:8097` 已失效，切换地址后客户端重新加载媒体库。

本次明确选择了二毛条目的本地 4K 标记 MKV 媒体源；Lux 收到的直放路径为脱敏后的 `/emby/Videos/:itemId/:mediaSourceId/stream.mkv`，没有请求 `.strm` 外部地址。客户端实际播放画面后，服务端结构化日志和 SQLite 均观察到：

| 流程 | 请求 | 状态/结果 |
|---|---|---|
| 建立播放 | `POST /emby/Sessions/Playing` | `204`，位置 0 |
| 播放进度 | 多次 `POST /emby/Sessions/Playing/Progress` | 均 `204`，位置从 `126000000` 增长至 `861670000` ticks |
| 停止播放 | `POST /emby/Sessions/Playing/Stopped` | `204`，最终状态 `STOPPED` |
| 客户端读回 | `GET /emby/Users/:userId/Items/Resume` | `200`；VidHub 详情页显示“继续播放” |

最终数据库记录绑定到该本地 MKV 的 `media_source_id`，`user_item_state.position_ticks=861670000`；播放会话的 `state=STOPPED`。该实测证明 VidHub 播放、退出停止和继续观看进度回传链路已打通。文件名中的 `2160p` 只属于媒体源标签，本机 ffprobe 对该夹具实际识别为 1920x1080 H.264，属于现有测试媒体内容差异。

SenPlayer 6.0.6 的历史实测结果：服务器已添加，但客户端重复请求 `POST /emby/Users/AuthenticateByName`，服务端均返回 `200`；客户端随后显示“未能读取数据，数据已丢失”，没有继续请求 `System/Info`。2026-08-06 真实 UI 重试捕获到认证后的 `GET /emby/Users/:userId`；该路由此前缺失，请求落入 Web 前端 fallback 并返回 HTML 200，正是客户端 JSON 解析失败的直接原因。补齐路由后，列表接口按请求的 `Fields` 省略未请求的 `MediaSources/MediaStreams`，并将服务监听到 SenPlayer 实际使用的 `192.168.50.108:8097`；真实 UI 已进入“我的媒体”，电影页显示 16 个条目，服务端总数为 22。

2026-08-07 SenPlayer 6.0.6 播放复测：客户端请求的脱敏路径为 `/emby/videos/:itemId/stream.mkv%3F...`，Lux 返回 `307` 并将 `.strm` 的外部地址放入 `Location`，不代理媒体字节；SenPlayer 播放器显示真实画面并以约 2.3 MB/s 读取，SQLite 播放会话记录为 `PLAYING`。未记录 token、Cookie、真实 `.strm` URL 或用户数据。

### 可重复的本地协议探针

`tools/compatibility-probe/probe.py` 可对本机 Lux 运行一次脱敏协议序列：

1. `System/Info/Public`
2. `Users/Public`
3. `Users/AuthenticateByName`
4. 带 token 的 `System/Info`、`System/Ping`
5. `Sessions/Logout`
6. logout 后再次访问 `System/Info`，应为 `401`

密码通过 `LUX_PROBE_PASSWORD` 注入，token 只在进程内使用；输出只包含路径、状态码和响应字段摘要。该工具用于协议回归，不等同于 VidHub、SenPlayer 或 Infuse 的真实客户端兼容性结论。
