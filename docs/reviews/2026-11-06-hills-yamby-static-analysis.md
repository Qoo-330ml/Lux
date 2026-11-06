# Hills 与 Yamby 对 Emby 服务端的静态依赖分析

日期：2026-11-06  
范围：Hills 1.11.0（versionCode 4139）与 1.8.0（4113）；Yamby 2.1.0.11 与 2.0.5.5  
性质：**只读静态分析**。未改源码，未执行解包目录中的脚本/二进制，未抓包，未接触手机或 adb。  
配套：真机抓包由另一任务记录在 `docs/reviews/2026-11-06-hills-yamby-capture.md`；本文是它的静态对照面，不替代实测。

## 0. 方法与限制

### 0.1 证据分级

| 标记 | 含义 |
|---|---|
| **确认** | 反编译 Java/Kotlin 代码、Moshi 适配器字段名或 Lux 路由表可直接读出 |
| **推断** | 只有 Dart AOT 字符串、类名、调用面或字段存在性等间接证据 |
| **无法确认** | 逻辑在 Dart AOT 快照或 native 保护方法体内，静态不可读 |

Hills 的 UI 与大部分 REST 调用在 Flutter（Dart AOT，`libapp.so`）里，只有播放器模块的 Java/Kotlin 层可读。因此本文把“Java 层确认”和“Dart 字符串推断”严格分开；混淆符号看不出语义时直接写“无法确认”，不猜。

### 0.2 路径缩写

| 缩写 | 实际路径（以 `/Volumes/Toshiba/mywork/客户端解包` 为根） |
|---|---|
| `H8` | `02-反编译/hills/1.8.0/jadx/sources` |
| `H11` | `02-反编译/hills/1.11.0/jadx/sources` |
| `HS8` / `HS11` | `05-附加产物/hills/{1.8.0,1.11.0}/strings/libapp.strings.txt`（行号为提取文件行号，首列是二进制偏移） |
| `Y8` | `02-反编译/yamby/2.0.5.5/sources-deobf` |
| `Y11` | `02-反编译/yamby/2.1.0.11/sources-deobf` |
| `L` | Lux 仓库（`src/api/emby.rs`、`src/api/routes.rs` 等） |

### 0.3 已知不可读点

- Hills 1.11.0 的 `mi/d0.invokeSuspend`（缩略图集合请求）被 jadx 标记 `JadxOverflowException`，方法体未还原。
- Hills 1.11.0 的 `a9/b.invokeSuspend` 同样被跳过；它是播放/会话请求的协程包装。
- Dart AOT 没有源码，只有字符串；路径拼接顺序、错误分支、并发顺序都不可读。
- Yamby 的 `com.hush.yamby.ui.activity.PlayerCmpActivity` 等方法体是 native（NMMP），其内部播放逻辑不可读；但 Emby 请求构造仍在可读的 `wc/`、`ac/`、`pd/` 层。

## 1. 结论速览（问题 1–8）

| # | 一句话结论 | 置信度 |
|---|---|---|
| 1 | Java 层只确认少数几个 Emby 接口（5 个具体路径 + 1 个通用会话 POST）；`Items/Resume`、`Items/Latest` 很可能是 `/Users/{uid}` 前缀（Lux 有），`Items/Counts` 1.8.0 是根路径（Lux 有）；`HideFromResume` 的形态无法确认，Lux 只有用户态路由；`IsResumable` 在 Hills 只见到字符串，无法确认是 `Filters` 值还是独立参数，Lux 只解析 `Filters` 形式 | 中（逐条见 §2） |
| 2 | `MediaSource` 1.11.0 读取 17 个键；`DirectStreamUrl` 非空优先，否则在“非 TV/非 STRM 直链 + 无 TranscodingUrl 或 SupportsTranscoding≠true”时自拼 `/Videos/{id}/stream.{container}?Static=true…&Tag={ETag}`；ETag 缺失时 1.8.0 发 `Tag=null`，1.11.0 只发裸 `Tag`（无 `=`），不会崩 | 确认（代码） |
| 3 | `Trickplay` 只有 Jellyfin 分支使用，Emby 走 `ThumbnailSet` + `/Items/{id}/Images/Thumbnail?...&PositionTicks=`；`Chapters` 用于跳过片头/片尾，缺 `ImageTag` 会触发“章节无效”启发式；`MediaSourceCount` 两个版本都不读；`AlternateMediaSources`/`TagItems` 只在 `Fields` 里请求，Kotlin 模型没有字段，Dart 用途无法确认 | 确认 + 无法确认 |
| 4 | `PlaybackBody` 固定发 20 个字段；`EventName ∈ {TimeUpdate, Pause, Unpause}`；`PlayMethod` 默认 `"DirectStream"`、`CanSeek` 默认 `true`、`PlaylistLength` 默认 1；Emby 协议首包 `Content-Type: text/plain`，415（或 500 + Emby NRE 文案）后重试 `application/json` 并记忆 | 确认（代码） |
| 5 | Hills 详情页的 Similar/AdditionalParts/Seasons/NextUp 全部是 Dart 调用，Java 层只发详情与剧集请求；没有 `Upcoming` 字符串；404/空响应的吞掉/提示/重试无法从静态代码确认 | 端点清单中；错误处理无法确认 |
| 6 | Java 播放层没有 401/403 分支；404 只对弹幕特判；415 会重试 JSON；其余异常抛出，可见的文案提取工具只在弹幕流程使用。Dart 层是否有全局 401 登出无法确认 | 确认（Java）/ 无法确认（Dart） |
| 7 | `/System/Ext/ServerDomains` 只存在于 Dart 字符串与本地 SQLite 路由列；404 时的具体分支不可读。从“notSupportServerRouting”等字符串看有“服务器不支持线路”的降级路径，但无法证明由 404 触发 | 推断（低-中） |
| 8 | Yamby 读取 `Similar`、`Shows/Upcoming`、`Videos/{id}/AdditionalParts`；`/Audio/{id}/stream` 只有构造器、无当前调用点；`MediaSourceCount` 不读；`DefaultSubtitleStreamIndex` 解析但未使用；WebSocket 失败后**无退避立即重连**且无任何提示 | 确认（代码） |

## 2. 问题 1：Retrofit 接口与 Lux 路由对照

### 2.1 Hills Java 层实际可读的接口面

Hills 的 Retrofit 风格接口在 1.8.0 是 `ii/a.java`，1.11.0 因 R8 改名变成 `ni/a.java`；URL 由调用方 `*0.b(serverUrl, protocol, path)` 拼好后传入（`H11/ei/s0.java:66-70`）。**接口本身只覆盖播放器需要的少数端点**，不是全部 Emby 面。

| 方法 | 路径（由调用方构造） | 查询参数（注解名） | 请求体 / 头 | 证据 |
|---|---|---|---|---|
| POST | `/Items/{itemId}/PlaybackInfo` | `IsPlayback`、`MediaSourceId`、`AudioStreamIndex`、`SubtitleStreamIndex`、`MaxStreamingBitrate`、`StartTimeTicks`、`CurrentPlaySessionId`；拦截器另加 `UserId` | body `{"DeviceProfile": {…}}`（`MaxStaticBitrate/MaxStreamingBitrate/MusicStreamingTranscodingBitrate/DirectPlayProfiles/TranscodingProfiles/ContainerProfiles/SubtitleProfiles`）；头 `User-Agent` | `H11/ni/a.java:80`；URL `H11/ni/l.java:88`；调用 `:100`；body 字段 `H11/DeviceProfileInfoJsonAdapter.java:24`、`DeviceProfileJsonAdapter.java:33`；`UserId` `H11/ni/f.java:50`；1.8.0 对应 `H8/ii/a.java:51` |
| POST | `/Sessions/Playing`、`/Sessions/Playing/Progress`、`/Sessions/Playing/Stopped`（均带 `?reqformat=json`） | `reqformat` | `PlaybackBody`；`Content-Type` 见 §5 | `H11/ni/a.java:39`；URL `H11/androidx/lifecycle/k0.java:633`；1.8.0 `H8/ii/r.java:94-96` |
| GET | `/Users/{userId}/Items/{itemId}` | `Fields`、`EnableImageTypes`、`ImageTypeLimit` | 头 `X-Cache-Mode: CACHE_FIRST` | `H11/ni/a.java:86`；`H11/ni/k.java:106` |
| GET | `/Shows/{seriesId}/Episodes` | `Fields`、`EnableImageTypes`、`ImageTypeLimit`、`SeasonId`（该方法内仅 `fnos` 协议非空） | — | `H11/ni/a.java:36`；`H11/ni/k.java:74` |
| GET | 缩略图集合（URL 未还原） | `MediaSourceId`、`Width` | — | `H11/ni/a.java:83`；方法体不可读 |
| GET | `/MediaSegments/{itemId}`（仅 Jellyfin） | `includeSegmentTypes` | — | `H11/ni/a.java:42`；`H11/ni/i.java:142` |
| HEAD/GET | 任意 URL（下载/流） | `Range` 头、`HeaderMap` | — | `H11/ni/a.java:49,77` |

补充（确认）：

- PlaybackInfo 返回的 `MediaSources` 按 `Id` 去重（`H11/ni/l.java:104-113`）。
- `MediaSources` 为空时会用 `IsPlayback=false` 自动重试一次（`H11/ni/m.java:59-65`）。这会额外产生一次 `POST PlaybackInfo?IsPlayback=false`。
- **1.11.0 的播放 API 客户端会给每个同源请求追加 `UserId` 查询参数**：`ni/f` 拦截器在 `H11/ni/f.java:49-50` 做 `yVarF.a("UserId", embyUserId)`，并在 `H11/ni/v.java:93-95` 挂到播放 API 客户端上。因此 Retrofit 注解里没有 `UserId`，实际请求里有；旧报告“Hills 不传 UserId”只对 1.8.0 成立（1.8.0 的 `ii/f` 只加 `X-Emby-*` 头，不做查询参数）。
- 协议前缀规则：`emby`/`fnos` → `/emby`；`jellyfin` → 无前缀（`H11/ei/s0.java:66-70`）。
- 令牌：相对 URL 会补 `api_key`（Emby/fnos）或 `ApiKey`（Jellyfin）；已是绝对 URL 则原样返回（`H11/ei/s0.java:39-60`、`H11/oi/v.java:210-236`）。
- 图片 URL：`/Items/{id}/Images/{type}?tag={tag}&quality=90&maxWidth=&maxHeight=`，尺寸向上取整到 100 的倍数（`H11/ei/s0.java:100-120`）。

### 2.2 端点对照表（Hills 用到的 Emby 路径 vs Lux）

“Hills 证据”列中：`J` = Java 代码确认；`S` = 仅 Dart 字符串；`S*` = 字符串 + 既有报告推断。Lux 列以 `L/src/api/emby.rs` 的注册表为准（根路径与 `/emby` 前缀同时挂载，且大小写不敏感）。

| # | Hills 请求（方法 + 路径） | Hills 证据 | Lux 路由 | 判定 | 影响备注 |
|---|---|---|---|---|---|
| 1 | GET `/System/Info/Public` | S（`HS11` 字符串存在） | `/System/Info/Public` | ✅ 有 | 匿名可访问 |
| 2 | POST `/Users/AuthenticateByName` | S | 同 | ✅ 有 | — |
| 3 | GET `/System/Ext/ServerDomains` | S（`HS11:7062`） | 无 | ❌ 无 | 见 §8 |
| 4 | GET `/Users/{uid}` | S | `/Users/{user_id}` | ✅ 有 | — |
| 5 | GET `/Users/{uid}/Views` | S | `/Users/{user_id}/Views` | ✅ 有 | — |
| 6 | GET `/Users/{uid}/Items` | S | `/Users/{user_id}/Items` | ✅ 有 | — |
| 7 | GET `/Users/{uid}/Items/Resume` | S*（`/Users/` + `/Items/Resume`） | `/Users/{user_id}/Items/Resume` | ✅ 有 | 若 Hills 走根路径见 #13 |
| 8 | GET `/Users/{uid}/Items/Latest` | S*（`/Users/` + `/Items/Latest`） | `/Users/{user_id}/Items/Latest` | ✅ 有 | 若走根路径见 #14 |
| 9 | GET `/Users/{uid}/Items/Counts` | S*（推断） | 无（只有根 `/Items/Counts`） | ❌ 无 | 若 Hills 用用户态则计数 404 |
| 10 | GET `/Users/{uid}/Items/{id}` | **J**（`H11/ni/k.java:106`） | `/Users/{user_id}/Items/{item_id}` | ✅ 有 | Lux 忽略 `X-Cache-Mode` |
| 11 | GET `/Items/{id}` | S | `/Items/{item_id}` | ✅ 有 | — |
| 12 | GET `/Items/{id}/Similar` | S（`HS11:23739`） | 无 | ❌ 无 | 详情页“相似”空 |
| 13 | GET `/Items/Resume`（根） | S*（`HS11:11616`） | 无 | ❌ 无 | 与 #7 二选一，无法确认实际用哪个 |
| 14 | GET `/Items/Latest`（根） | S*（`HS11:23990`） | 无 | ❌ 无 | 与 #8 二选一 |
| 15 | GET `/Items/Counts`（根） | S（1.8.0 有完整 `/emby/Items/Counts`，`HS8:25020`） | `/Items/Counts` | ✅ 有 | 接受 `UserId` 查询 |
| 16 | GET `/Items/{id}/AdditionalParts` | S（`HS11:26950`） | 无 | ❌ 无 | “额外片段”空 |
| 17 | GET `/Items/{id}/Seasons` 或 `/Shows/{id}/Seasons` | S*（`HS11:25097`） | 仅 `/Shows/{series_id}/Seasons` | ⚠️ 形态不同 | 若用 `/Items/{id}/Seasons` 则 404 |
| 18 | GET `/Shows/{seriesId}/Episodes` | **J**（`H11/ni/k.java:74`） | `/Shows/{series_id}/Episodes` | ✅ 有 | 该方法内仅 fnos 传 `SeasonId`（调用点未确认） |
| 19 | GET `/Shows/NextUp` | S | `/Shows/NextUp` | ✅ 有 | — |
| 20 | POST `/Items/{id}/PlaybackInfo` | **J**（`H11/ni/a.java:80`） | 同（GET+POST） | ✅ 有 | Lux 同时支持 GET/POST |
| 21 | GET `/Videos/{id}/stream.{container}` | **J**（`H11/androidx/lifecycle/s0.java:237`；1.8.0 `H8/ji/r.java:822`） | `/Videos/{item_id}/stream.{container}` | ✅ 有 | 大小写不敏感 |
| 22 | POST `/Sessions/Playing` | **J**（`H11/androidx/lifecycle/k0.java:633`） | 同 | ✅ 有 | — |
| 23 | POST `/Sessions/Playing/Progress` | **J** | 同 | ✅ 有 | `EventName` 未知字段会被忽略 |
| 24 | POST `/Sessions/Playing/Stopped` | **J** | 同 | ✅ 有 | — |
| 25 | GET `/Sessions` | S | `/Sessions` | ✅ 有 | — |
| 26 | POST/DELETE `/Users/{uid}/PlayedItems/{id}` | S | 同 | ✅ 有 | — |
| 27 | POST/DELETE `/Users/{uid}/FavoriteItems/{id}` | S | 同 | ✅ 有 | — |
| 28 | POST `/Items/{id}/HideFromResume?Hide=true` | S*（`HS11:14353`） | 仅 `/Users/{user_id}/Items/{item_id}/HideFromResume` | ❌ 无（根路径） | **若 Hills 用根路径则隐藏功能静默 404** |
| 29 | POST `/Items/{id}/Refresh` | S | 同 | ✅ 有 | 需管理员 |
| 30 | DELETE `/Items/{id}` | S | 同 | ✅ 有 | — |
| 31 | POST `/Items/Delete`（批量） | S | 无 | ❌ 无 | 管理端批量删除 |
| 32 | GET `/Persons`、`/Genres`、`/Studios`、`/Years` | S | 同 | ✅ 有 | — |
| 33 | GET `/Tags`、`/OfficialRatings` | S | 无 | ❌ 无 | 筛选维度缺失 |
| 34 | GET `/Localization/cultures` | S | 无 | ❌ 无 | 语言列表可能空 |
| 35 | GET `/LiveTv/Channels` | S | 无 | ❌ 无 | 直播库不可用 |
| 36 | GET `/Library/VirtualFolders` | S | 同（仅 GET） | ⚠️ 无 DELETE | Hills 还有 DELETE 版本 |
| 37 | GET `/Items/{id}/Images/{type}` | **J**（`H11/ei/s0.java:100-120`） | 同 | ✅ 有 | — |
| 38 | GET `/Items/{id}/Images/Thumbnail?...&PositionTicks=&MediaSourceId=` | **J**（`H11/oi/t.java:1260`） | `/Items/{item_id}/Images/{image_type}` | ⚠️ 形态不同 | `Thumbnail` 被映射为 `THUMB`，`PositionTicks` 被忽略；预览图退化为普通 Thumb |
| 39 | GET `/Videos/{id}/Trickplay/{width}/{index}.jpg` | **J**（`H8/PreviewThumbnailData.java:160`；1.11.0 同） | 无 | ❌ 无 | Jellyfin 协议分支；Emby 协议不走这条 |
| 40 | GET 缩略图集合（路径未还原，`MediaSourceId`+`Width`） | **J**（接口）/ 方法体不可读 | 无 | ❌ 无（或无法确认） | 拖动条预览可能空 |
| 41 | GET `/MediaSegments/{id}` | **J**（仅 jellyfin，`H11/ni/i.java:142`） | 无 | ❌ 无 | Emby 协议不调用 |
| 42 | GET `/Items/{id}/Download` | S | 同 | ✅ 有 | — |
| 43 | GET `/Items/{id}/RemoteSearch/Subtitles/{lang}`、`/Providers/Subtitles/Subtitles/{id}`、POST `/Items/RemoteSearch/…` | S | 无 | ❌ 无 | 在线字幕/元数据搜索不可用 |

### 2.3 四个重点端点

| 问题 | 结论 | 证据 | 置信度 |
|---|---|---|---|
| `Items/Resume` 是否带 `/Users/{id}` 前缀 | **推断带**。1.11.0 只有 `/Users/`（`HS11:7801`）与 `/Items/Resume`（`HS11:11616`）两个片段；1.8.0 有完整 `/emby/Users/`（`HS8:25545`）+ `/Items/Resume`（`HS8:11019`）。Dart AOT 里没有完整模板，拼接顺序不可见 | `HS11`/`HS8` 上述行；`H11/ei/s0.java:66-70` 只是前缀规则 | 中高（推断） |
| `Items/Latest` 同上 | **推断带**（同样的 `/Users/` + `/Items/Latest` 片段） | `HS11:23990`、`HS8:22764` | 中高（推断） |
| `Items/Counts` 是否带 `/Users/{id}` | **1.8.0 确认是根路径**：存在完整字面量 `/emby/Items/Counts`（`HS8:25020`），没有 `Users` 段。1.11.0 只剩 `/Items/Counts`（`HS11:23146`），仍更可能是根路径 | `HS8:25020`、`HS11:23146`；Lux 路由只有根路径 | 高（1.8.0 确认）/ 中（1.11.0） |
| `HideFromResume` 形态 | **无法确认**。字符串只有 `/HideFromResume`（`HS8:13647`、`HS11:14353`）。旧报告写的是根路径 `Items/{id}/HideFromResume`，但同一份报告对 `Resume` 也写成了根路径，二者至少有一个是拼接推断。Lux 只注册了 `/Users/{uid}/Items/{id}/HideFromResume` | `L/src/api/emby.rs:224`；`HS11:14353` | 低（无法确认） |
| `IsResumable` 是 `Filters` 值还是独立参数 | **无法确认**。只确认字符串存在（`HS8:24520`、`HS11:25859`），旁边没有 `Filters=` 组合的证据。Lux 只解析 `Filters` 的 `IsResumable`，不解析独立的 `IsResumable=true` | `L/src/api/emby_catalog.rs:333-388`；`HS11:25859` | 低（无法确认） |

**对 Lux 的直接结论**：四个端点里只有 `Items/Counts`（根）和 `Items/Resume`/`Items/Latest`（用户态）是稳的；`HideFromResume` 的根路径别名和 `IsResumable` 独立参数是两个真实风险点。注册根路径别名成本很低，建议覆盖两种形态而不是赌一种。

## 3. 问题 2：`MediaSource` 字段与直链拼接

### 3.1 Moshi 适配器字段集

| 版本 | 字段（顺序即适配器 options 顺序） | 证据 |
|---|---|---|
| 1.8.0 | `Protocol, Id, Path, Type, Container, Size, Name, RunTimeTicks, MediaStreams, Bitrate, DirectStreamUrl, ETag, TranscodingUrl, SupportsDirectStream, SupportsTranscoding`（15） | `H8/com/mountains/player/models/MediaSourceJsonAdapter.java:35` |
| 1.11.0 | 上述 15 个 + `DisplayTitle`、`AddApiKeyToDirectStreamUrl`（17） | `H11/com/mountains/player/models/MediaSourceJsonAdapter.java:32` |

结论（确认）：

- **没有** `MediaSourceCount`、`SupportsDirectPlay`、`DefaultAudioStreamIndex`、`DefaultSubtitleStreamIndex`、`AlternateMediaSources`。
- `AddApiKeyToDirectStreamUrl` 只被反序列化和 `toString` 读取，**没有任何业务分支使用**（全仓库仅 `MediaSource.java` 的 getter/copy/toString 和适配器出现）。这解释了为什么 Lux 改这个字段对 Hills 无效。

### 3.2 直链决策（1.8.0 与 1.11.0 语义一致）

1.8.0：`H8/ji/r.java:809-825`；1.11.0：`H11/androidx/lifecycle/s0.java:187-260`（`oi/v` 的 `EmbyPlayableSource`，`toString` 在 `H11/oi/v.java:238-240` 给出字段语义：`tvChannelDirectLinkPlay`、`strmDirectLinkPlay`、`isTvChannel`、`serverUrl`、`itemId`、`deviceId`、`accessToken`、`protocol`）。

| 优先级 | 条件 | 结果 |
|---|---|---|
| 1 | `isTvChannel && tvChannelDirectLinkPlay && Path` 是 http(s) URL | 直接用 `Path` |
| 2 | `!isTvChannel && strmDirectLinkPlay && Path` 是 http(s) URL | 直接用 `Path` |
| 3 | `DirectStreamUrl` 非空（1.8.0 只判 `!= null`；1.11.0 还判非空白） | 用服务端的 `DirectStreamUrl`；相对 URL 经 `k(url)` 补 `api_key`，绝对 URL 原样使用 |
| 4 | `TranscodingUrl` 非空且 `SupportsTranscoding == TRUE` | 用 `TranscodingUrl` |
| 5 | 仅非 TV 分支，且以上都不满足 | 自拼 `/Videos/{itemId}/stream.{container}?Static=true&mediaSourceId=…&deviceId=…&api_key=…&Tag={ETag}` |

TV 分支与非 TV 分支的兜底不同：TV 分支在 `DirectStreamUrl`/`TranscodingUrl` 都不满足时退回 `Path`，不会自拼；只有非 TV 分支才走第 5 步。

自拼分支的细节：

- `container` 为空 → 后缀为空，路径变成 `/stream`（没有扩展名），不是崩溃。
- 1.11.0 对 `fnos` 协议把 container 转大写，路径用 `/emby/videos`（小写 v）；对 emby/jellyfin 用 `/Videos`。1.8.0 没有 fnos 分支。
- 1.11.0 的查询参数名对 `fnos` 用 `MediaSourceId`/`DeviceId`，其余用 `mediaSourceId`/`deviceId`；令牌对 jellyfin 用 `ApiKey`，其余 `api_key`（`H11/androidx/lifecycle/s0.java:248-252`）。
- 1.8.0 用字符串拼接，路径硬编码 `/Videos`，参数名固定 `mediaSourceId`/`deviceId`/`api_key`。

### 3.3 `ETag` 缺失时的行为

| 版本 | 行为 | 证据 |
|---|---|---|
| 1.8.0 | 字符串拼接 `"&Tag=" + mediaSource.getETag()`；`ETag == null` 时发出字面量 **`&Tag=null`** | `H8/ji/r.java:822` |
| 1.11.0 | 走 `xe.y`（OkHttp `HttpUrl.Builder` 的等价实现）的 `addQueryParameter("Tag", null)`；值为 `null` 时 `xo.b.b` 只写参数名、不写 `=`，最终是 **裸 `Tag`**；`ETag == ""` 时是 `Tag=` | `H11/androidx/lifecycle/s0.java:252`；`H11/xe/y.java:74-90`；`H11/xo/b.java:26-46` |

不会空指针、不会崩溃：两条路径都把 `null` 当普通值处理。服务端需要容忍 `Tag=null`、裸 `Tag`、`Tag=` 三种形态；Lux 的 `/Videos/.../stream` 不解析 `Tag`，因此无影响。真正的风险只在“服务端把 `Tag` 当 ETag 严格校验”的实现上。

### 3.4 与 Lux 的差异

- Lux 的 `MediaSource` **不返回 `ETag`**（`L/src/api/emby_catalog.rs:4720-4760` 的 JSON 里没有该键），也**不返回 `AlternateMediaSources`/`MediaSourceCount`**。
- Lux 的 `DirectStreamUrl` 是**相对路径**（`/Videos/{id}/stream…?MediaSourceId=`，`L/src/api/emby_catalog.rs:4812`），Hills 会补 `/emby` 前缀和 `api_key` 后使用；这条链路是通的。
- Lux 返回 `SupportsTranscoding: true`（本地文件可转码时）且 `TranscodingUrl` 只在有转码会话时存在；Hills 只有在 `DirectStreamUrl` 为空时才会看 `TranscodingUrl`，所以正常直放不受影响。
- `AddApiKeyToDirectStreamUrl` 对 Hills 无意义（见 §3.1），不要指望它改变 Hills 行为。

## 4. 问题 3：`AlternateMediaSources` / `MediaSourceCount` / `Trickplay` / `TagItems` / `Chapters`

| 字段 | Hills 是否请求 | Hills 是否解析 | UI 用途 / 缺失降级 | 证据 | 置信度 |
|---|---|---|---|---|---|
| `AlternateMediaSources` | 是（两档 Fields） | Kotlin 模型**没有**该字段 | Dart 侧用途无法确认；详情页可能不显示“其他版本” | `HS11:12760,17590`；`H11/com/mountains/player/models/ItemJsonAdapter.java:45`、`MediaSourceJsonAdapter.java:32` 均无 | 请求确认；用途无法确认 |
| `MediaSourceCount` | 否（字符串不存在） | 否 | 无影响 | `HS8`/`HS11` 全文无该串；两个适配器无该键 | 确认 |
| `Trickplay` | 是（Fields） | 是：`Item.trickplay` 类型为 `Map<mediaSourceId, Map<width, JellyfinTrickplayInfo>>`；`JellyfinTrickplayInfo` 读 `Width/Height/TileWidth/TileHeight/ThumbnailCount/Interval/Bandwidth` | **仅 Jellyfin 协议**用它算拖动条预览；Emby 协议改走 `ThumbnailSet` API + `/Items/{id}/Images/Thumbnail?...&PositionTicks=&MediaSourceId=`。`Trickplay` 缺失时 map 为 null，走 Emby 分支；`ThumbnailSet` 失败则没有预览图，不崩 | `H11/ItemJsonAdapter.java:45,60`；`H11/JellyfinTrickplayInfoJsonAdapter.java:28`；`H11/oi/f.java:119-140`；`H11/oi/t.java:1216-1260` | 确认（解析）/ 路径未确认（ThumbnailSet URL） |
| `TagItems` | 是（Fields） | Kotlin 模型**没有**该字段 | Dart 侧用途无法确认；可能用于标签筛选/展示 | `HS11:12760,17590`；`ItemJsonAdapter.java:45` 无 `TagItems` | 请求确认；用途无法确认 |
| `Chapters` | 是（完整 Fields 档） | 是：`Chapter` 读 `ChapterIndex/Name/StartPositionTicks/MarkerType/ImageTag`；`MarkerType ∈ {Chapter, IntroStart, IntroEnd, CreditsStart}` | 用于跳过片头/片尾和章节列表。缺失/空 → 不显示跳过按钮；**没有 `ImageTag` 且章节恰好每 5 分钟一条时，`isInvalidChapters()` 会把整组章节判为无效**，从而不提供跳过片头 | `H11/ChapterJsonAdapter.java:32`；`H11/MarkerType.java`；`H11/qi/l1.java:141-170`；`H11/com/mountains/player/models/Item.java:637-669` | 确认 |

补充：

- `Chapters` 的 `ImageTag` 启发式值得注意：Lux 的 `emby_chapter_json` **不返回 `ImageTag`**（`L/src/api/emby_catalog.rs:4908-4924`），只返回 `StartPositionTicks`、`MarkerType`、`ChapterIndex`、`Name`。如果章节是均匀的 5 分钟网格，Hills 会把它们判为“无图章节”并忽略，即使 Lux 已经给了 `IntroStart`/`CreditsStart`。这不是播放失败，但会让“跳过片头”静默失效。
- Emby 协议的 `Trickplay` 走的是 `ThumbnailSet`；该请求的 URL 在 jadx 里没有还原（`H11/mi/d0.invokeSuspend` 方法体缺失），因此无法确认 Hills 请求的是 `/Items/{id}/Thumbnails`、`/Videos/{id}/Thumbnails` 还是别的路径。Lux 没有对应端点，最可能的结果是 404 → 无预览图。

## 5. 问题 4：会话上报（`PlaybackBody`）

### 5.1 字段、来源与默认值

`PlaybackBody` 的 Moshi 适配器固定序列化 20 个键（`H11/com/mountains/player/models/PlaybackBodyJsonAdapter.java:33,511-557`）。构造工厂 `H11/qi/v0.java:41` 用合成构造器的默认掩码 `954550` 补齐未传参数；默认值在 `H11/com/mountains/player/models/PlaybackBody.java:349`。

| JSON 键 | 实际值 / 来源 | 必填性 | 证据 |
|---|---|---|---|
| `EventName` | `TimeUpdate` / `Pause` / `Unpause`；适配器可空 | 可空；三个调用点都会设 | `H11/EventName.java`；`H11/oi/t.java:477,568` |
| `VolumeLevel` | 默认 **100**（工厂传 0，被默认掩码覆盖） | 非空 int | `PlaybackBody.java:349` |
| `IsMuted` | 默认 `false` | 非空 bool | 同上 |
| `IsPaused` | 播放器暂停状态 | 非空 bool | `v0.a` 参数 |
| `RepeatMode` | 默认 **`"RepeatNone"`** | 非空 string | 同上 |
| `SubtitleOffset` | 默认 0 | 非空 int | 同上 |
| `PlaybackRate` | 当前倍速 | 非空 double | `v0.a` 参数 |
| `MaxStreamingBitrate` | 默认 **140000000**（工厂传 0，被默认覆盖） | 非空 long | 同上 |
| `PositionTicks` | 当前播放位置（100ns） | **服务端应视为必填** | `v0.a` 计算 |
| `PlaybackStartTimeTicks` | 墙钟毫秒 × 10000 | 非空 long | `v0.a` |
| `SubtitleStreamIndex` | 当前字幕轨 | 非空 int | `v0.a` |
| `AudioStreamIndex` | 当前音轨 | 非空 int | `v0.a` |
| `PlayMethod` | 默认 **`"DirectStream"`**（工厂传 null，被默认覆盖） | 非空 string | `PlaybackBody.java:349` |
| `PlaySessionId` | 来自 `PlayStatus`；null → `""` | 可空串（发空串） | `v0.a` |
| `MediaSourceId` | 来自 `PlayStatus`；null → `""` | 可空串（发空串） | `v0.a` |
| `CanSeek` | 默认 **`true`** | 非空 bool | `PlaybackBody.java:349` |
| `ItemId` | 必有 | **必填** | 构造函数 `checkNotNull` |
| `PlaylistIndex` | 默认 0 | 非空 int | 同上 |
| `PlaylistLength` | 默认 **1** | 非空 int | 同上 |
| `Shuffle` | 默认 `false` | 非空 bool | 同上 |

1.8.0 多一个 `NowPlayingQueue`（1.11.0 已删除）：`H8/com/mountains/player/models/PlaybackBodyJsonAdapter.java:134`；1.8.0 的工厂同样用默认掩码（`H8/li/m0.java:40`）。

### 5.2 端点与 `Content-Type`（重要）

| 事件 | 端点 | 证据 |
|---|---|---|
| 开始 | `POST /Sessions/Playing?reqformat=json` | `H11/qi/u0.java:89`（suffix null） |
| 暂停/恢复 | `POST /Sessions/Playing/Progress?reqformat=json` | `H11/qi/u0.java:73`；`EventName=Pause/Unpause` |
| 进度 | `POST /Sessions/Playing/Progress?reqformat=json` | `H11/qi/u0.java:106`；`EventName=TimeUpdate`，每 10 秒或倍速阈值（`H11/oi/t.java:563`） |
| 停止 | `POST /Sessions/Playing/Stopped?reqformat=json` | `H11/qi/u0.java:122` |

`Content-Type` 选择（确认）：

- Jellyfin / fnos / 该请求键已记录为“需要 JSON” → 直接 `application/json`（实现上键是 serverUrl，不是完整 URL）。
- 其余（含 Emby）→ **先发 `text/plain`**；若抛 `sp.r`（HTTP 异常）：
  - `415` → 用 `application/json` 重试，并把该请求键加入进程内集合，后续直接 JSON；
  - `500` 且响应体前 4096 字节包含 `"Object reference not set to an instance of an object."` → 同样重试 JSON；
  - 其它状态码 → 重新抛出。
- 证据：`H11/mg/g.java:164-280`（415 在 `:211`，500 文案在 `:213-217`，重试在 `:228`）。
- 1.8.0 没有这层回退，只是 `isJellyfin ? application/json : text/plain`，且会话 URL **硬编码 `/emby` 前缀**（`H8/ii/r.java:94-96`）——这意味着 1.8.0 在 Jellyfin 模式下会话上报会打到 `/emby/...`，是一个客户端 bug；1.11.0 才改为按协议加前缀。

对 Lux 的含义：Lux 用 `Bytes` + `serde_json` 解析 body，不校验 `Content-Type`（`L/src/api/playback.rs:1411-1419`），所以 Hills 的 `text/plain` 首包可以直接成功，不会触发 415 重试；`reqformat=json` 是未知查询参数，被忽略。这一点已在 `docs/COMPATIBILITY.md` 的 2026-09-05 条目记录，静态分析与之一致。

## 6. 问题 5：Hills 详情页的并发请求与 404/空响应

### 6.1 可确认的请求

| 请求 | 谁发起 | 证据 | 说明 |
|---|---|---|---|
| `GET /Users/{uid}/Items/{id}` | Java（播放器） | `H11/ni/k.java:106` | `Fields=ProviderIds,ExternalUrls,PrimaryImageAspectRatio,CommunityRating,PremiereDate,Genres,Overview,ProductionYear,Trickplay`；头 `X-Cache-Mode: CACHE_FIRST` |
| `GET /Shows/{seriesId}/Episodes` | Java（播放器） | `H11/ni/k.java:74` | `Fields=Chapters,ProviderIds,…,Trickplay`；该方法内 `SeasonId` 仅当协议为 fnos 时非空（`H11/ni/k.java:76`），调用点未确认 |
| `GET /Items/{id}/Similar` | Dart | `HS11:23739` | 调用点不可读 |
| `GET /Items/{id}/AdditionalParts` | Dart | `HS11:26950` | 调用点不可读 |
| `/Seasons` | Dart | `HS11:25097` | 可能是 `/Shows/{id}/Seasons` 或 `/Items/{id}/Seasons`，无法确认 |
| `GET /Shows/NextUp` | Dart | `HS11:23903` | 调用点不可读 |
| `GET /Items/{id}`（另一条详情路径） | Dart | `HS11` 的 `/Items`、`/Items/` 片段 | 与 Java 的 `/Users/{uid}/Items/{id}` 并存；实际详情页用哪条无法确认 |

### 6.2 并发与错误处理

- **并发清单无法确认**：这些请求都在 Dart AOT 里，Java 层没有对应协程；字符串只能证明路径存在，不能证明“同时发”或顺序。
- **`Upcoming` 不存在**：`HS8` 与 `HS11` 全文都没有 `Upcoming` 字符串（`grep -a Upcoming` 无结果）。Hills 不请求“即将播出”。
- **404/空响应的处理无法确认**：
  - Java 侧对 Emby 请求没有 404 特判；唯一的 `sp.r.C == 404` 分支在弹幕流程（`H11/qi/a0.java:45`、`H11/qi/d0.java:59`）。
  - Java 侧可见的错误文案提取工具是 `ei/s1.g(Throwable)`（`H11/ei/s1.java:236`），实际调用点只在弹幕/匹配流程。
  - Dart 侧无法读到 catch/重试逻辑；`Load Failed`、`retry` 等字符串存在但无法绑定到具体页面。
- 对 Lux 的实际影响：如果 Hills 的详情页对 404 是“空 section”处理，则缺失 `Similar`/`AdditionalParts` 只表现为空块；如果是“整页错误”，则详情页可能报错。**这一条必须靠抓包报告或真机确认**，静态分析给不出结论。

## 7. 问题 6：状态码分支

| 状态码 | Hills Java 层行为 | 证据 | 置信度 |
|---|---|---|---|
| 401 | **无专用分支**。播放器请求失败后按通用异常处理；没有登出、没有跳登录页的代码。认证头由 Dart 传入并在 `ni/f.java:54` 透传（`X-Emby-*`），Java 不做刷新 | `H11/ni/f.java:54`；Java 层未发现 `== 401` 分支 | 确认（Java）/ Dart 无法确认 |
| 403 | 无专用分支；与 401 一样走通用异常 | 同上 | 确认 |
| 404 | **仅弹幕**有特判（返回空列表而不是失败）；Emby 播放/详情请求没有 404 分支 | `H11/qi/a0.java:45`、`H11/qi/d0.java:59` | 确认 |
| 415 | 会话上报专用：`text/plain` 失败后改 `application/json` 重试并记住 URL | `H11/mg/g.java:211-230` | 确认 |
| 500 | 仅当响应体包含 Emby 的 `"Object reference not set to an instance of an object."` 时重试 JSON；其他 500 原样抛出 | `H11/mg/g.java:213-217` | 确认 |
| 其它 4xx/5xx | 抛出 `sp.r`（`RuntimeException` 子类，带状态码与响应），由上层协程处理；UI 文案由 `ei/s1.g` 提取响应体 | `H11/sp/r.java`；`H11/ei/s1.java:236-270` | 确认 |

补充：`f1/j4.java:56` 有一个“重试 < 3 次且不是 404”的谓词，但它服务于图片/请求重试的通用逻辑，不是 Emby API 的登出分支。

Dart 层：`Unauthorized` 字符串存在（`HS11:22667`），但同一文件里还有 Supabase/Trakt/Simkl 的 `*RetriedAfterUnauthorized`（`HS11:15556,16644,22416`），无法证明 Emby 401 会触发登出。**结论：无法确认**。

对 Lux 的建议（只列建议）：401 与 403 要严格区分；不要把“权限不足”返回 401，否则可能触发客户端的令牌失效逻辑（即使 Hills 静态看不到，Yamby 的 401 分支是存在的，见 §9.7）。

## 8. 问题 7：多线路 / `System/Ext/ServerDomains`

| 证据 | 内容 |
|---|---|
| `HS8:18040` / `HS11:7062` | 字符串 `/emby/System/Ext/ServerDomains` / `/System/Ext/ServerDomains` |
| `HS8:6562` / `HS11:6943` | `supportServerRouting` |
| `HS8:13427` / `HS11:14104` | `notSupportServerRouting` |
| `HS11:16442,17376,23609,25277` | SQLite `ALTER TABLE "emby_servers"` 的 `main_routing_name`、`server_routing_list`、`enable_failover`、`selected_server_routing_id` 列 |
| `HS11:17052` | `Invalid Emby server routing order` |
| `HS11:18489` | `Unlock Pro to support server routing switch` |

结论：

- **确认**：Hills 有“服务器线路/域名池”功能，数据落在本地 SQLite（`server_routing_list`、`main_routing_name`、`selected_server_routing_id`、`enable_failover`），并有一个 `/System/Ext/ServerDomains` 探测请求。
- **推断（低-中）**：该请求只在用户配置多线路/开启 failover 时发出；404 会被当成“服务器不支持”，UI 显示 `notSupportServerRouting` 或不展示线路入口，然后继续用主地址。依据是这两条字符串与默认 `server_routing_list = "[]"`、`enable_failover = 0`，以及没有 Java 侧处理代码（说明在 Dart 里）。
- **无法确认**：404 是否被 catch、是否重试、是否阻塞首屏。静态代码看不到 Dart 分支。旧报告“返回 404 也能用”目前只能算推断，不能算代码确认。
- 对 Lux 的建议：保持 404（不要 500、不要超时、不要挂起）；如果未来要支持多线路，再实现该端点。优先级低。

## 9. 问题 8：Yamby

Yamby 2.1.0.11 有完整的可读源码树（`Y11`），以下均为代码确认，除非另注。

### 9.1 `Similar`

- 调用点 1：详情页 ViewModel `Y11/pd/C5013.java:786`，`GET /emby/Items/{id}/Similar?Limit=10&UserId={uid}&ImageTypeLimit=1&Fields=BasicSyncInfo,CanDelete,CanDownload,Container,PrimaryImageAspectRatio,ProductionYear,EndDate,Status,CommunityRating,OfficialRating&EnableTotalRecordCount=false`，请求构造里设置了 86400000ms 的缓存参数（`:790` 附近的 `m9855(86400000L)`）。
- 调用点 2：播放器“推荐” `Y11/ac/C0165.java:252`，同样的 `/Items/{id}/Similar`，参数名由 `AbstractC5938.m11061` 解密后拼装（`Limit`、`ImageTypeLimit`、`Fields`），`UserId` 取当前配置，`EnableTotalRecordCount=false`。
- 失败降级：`pd/C5013.m9562` 用 `catch (Throwable)` 包住整个请求，异常只交给 `AbstractC7070.m12774`（包装成 `Result.failure`）后丢弃，状态机不更新 → **静默失败，section 为空**。`ac/C0165.m490` 没有本地 catch，异常向上抛。
- Lux：无 `/Items/{id}/Similar` → 404 → 两个调用点都拿不到数据；详情页“相似”为空，播放器推荐为空。**不崩**。

### 9.2 `Upcoming`

- 调用点：`Y11/pd/C5066.java:77`，`GET /emby/Shows/Upcoming?Limit=60&UserId={uid}&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Thumb&ParentId={libraryId}&EnableTotalRecordCount=false`。
- 失败降级：`mo8016` 没有本地 catch，异常抛给分页/列表基类；没有“空列表代替”的分支。具体 UI 表现取决于基类（未继续下钻），但至少不是静默吞掉。
- Lux：无 `/Shows/Upcoming` → 404；该库的“即将播出”视图会失败/空白。

### 9.3 `AdditionalParts`

- 调用点：`Y11/pd/C5013.java:141`，`GET /emby/Videos/{id}/AdditionalParts`（**注意是 `Videos`，不是 `Items`**），无查询参数。
- 失败降级：同样 `catch (Throwable)` → 静默，`list` 保持 `null` 并归一化为空列表（`C3603.f16259`）。
- Lux：无 `/Videos/{id}/AdditionalParts` → 404 → “额外片段”为空；不影响正片播放。

### 9.4 `/Audio/{id}/stream`

- 构造器：`Y11/com/hush/yamby/data/bean/EmbyPlaybackInfo.java:95`，`{base}{/emby}/Audio/{itemId}/stream{container}?api_key={token}&Static=true&MediaSourceId={msId}`。
- 触发条件：`m4315` 的 `z13 = (mask & 32) == 0` 时进入音频分支；视频分支是 `mask & 16 == 0` 或 `mask & 32 != 0`。
- **实际调用点**：2.1.0.11 全部可读的 `m4315(...)` 调用的最后一个 int 实参只有 `24/32/36/48/52/56`，**全部包含 bit 32**，因此音频分支在当前调用图里不可达。2.0.5.5 的 `m3751` 同样只有 `24/32/36/48/52`，也不可达。
- 结论：`/Audio/{id}/stream` 是**保留但未使用的构造器**；Lux 不实现它不会影响 Yamby 2.1.0.11 的现有播放路径。若未来版本启用，Lux 需要该路由。置信度：高（穷举 grep + 掩码分析）。

### 9.5 `MediaSourceCount` 与 `DefaultSubtitleStreamIndex`

- `MediaSourceCount`：**2.1.0.11 与 2.0.5.5 的可读源码里都没有**（`grep -rn MediaSourceCount` 无结果）。Yamby 的 `EmbyMediaSources` 序列化器只注册 14 个键（`Y11/oc/C4742.java:43-50`），没有它。结论：Yamby 不读，Lux 不返回无影响。
- `DefaultSubtitleStreamIndex` / `DefaultAudioStreamIndex`：解析，默认值 `-1`（`Y11/oc/C4742.java:48-49`），但字段 `f6019`/`f6020` 只在序列化器的构造调用里出现（`oc/C4742.java:166-167`），**没有任何轨道选择逻辑读取**。Yamby 自己决定默认轨（`EmbyMediaSources.m4296/m4297` 等方法按 `IsDefault`/类型挑选）。结论：Lux 不返回这两个字段无影响；返回也不会改变 Yamby 的默认轨选择。置信度：高。

### 9.6 WebSocket（`/embywebsocket`）

调用链（确认）：

1. `Y11/mc/C4000.java:86-99`：把服务器 base URL 的 `https://`→`wss://`、`http://`→`ws://`，拼 `"{base}/embywebsocket?api_key={token}&deviceId={deviceId}"`，设置 listener，`m11763()` 连接。**路径固定为 `/embywebsocket`，不按 Emby/Jellyfin 协议区分，也不加 `/emby` 前缀**。
2. `Y11/wc/C6328.java:70-146`：每次 `m11763()` 新建一个 OkHttp `Dispatcher`、构造 `C0378` 并 `m9795(...)` 异步连接；没有退避、没有次数上限。
3. `Y11/r8/C5326.java:543-580`：
   - `onClosed`（`m9948`）：`f27373 == true` → 立即 `m11763()`；
   - `onFailure`（`m9950`）：同样 `f27373 == true` → 立即 `m11763()`。
   - `f27373` 在 `C4000` 构造时置 true（`mc/C4000.java:98`），只在 ViewModel `onCleared` 时置 false（`mc/C4000.java:104-107`）。
4. `Y11/a0/C0004.java:251-253`：`mo66(Exception)` **方法体为空** → 连接失败没有任何 Toast/Snackbar/日志。
5. `Y11/a0/C0004.java:241-247`：连接成功后主动发 `{"MessageType":"ScheduledTasksInfoStart","Data":"1000,1000"}` 与 `{"MessageType":"SessionsStart","Data":"0,1500,0,true,true"}`。
6. `Y11/a0/C0004.java:1377-1420`：消息解析；只处理 `ScheduledTasksInfo` 和 `Sessions`；`Sessions` 条目缺 `NowPlayingItem` 或 `PlayState` 时被过滤（与旧报告一致）。
7. 使用范围：`C4000` 在 `hc/AbstractC2788.m6033`（服务器状态/日志入口的 Compose 页面）创建；不是全局 App 级连接，但该页面存活期间会一直重连。

对 Lux 的含义（重要）：

- Lux **没有任何 WebSocket 路由**（`grep -ri websocket src/` 无结果）。`/embywebsocket` 会落到 SPA fallback 或 Emby 命名空间 404；OkHttp 期望 101，收到 200/404 都会触发 `onFailure`。
- 因为 `onFailure → 立即重连` 且无退避，Yamby 在该页面会形成**紧凑重连循环**：每次失败新建 Dispatcher、重新握手，日志/CPU/电量都会被拖累；没有用户提示。
- 如果 Lux 只是返回 404，循环仍然存在，只是每次失败的代价更小；要彻底避免，需要实现握手（至少返回 101 并保持连接/发送空消息），或者接受“该页面会持续重试”的现状并在文档中说明。
- 另外，Yamby 连上后会发 `SessionsStart`/`ScheduledTasksInfoStart`；如果 Lux 将来实现 WebSocket，要能接受这两条客户端消息并忽略未知类型。

### 9.7 Yamby 的状态码处理（补充）

- 401：`Y11/pd/C5025.java:559` 有分支（`C4860.f20651 == 401` 时进入事件上报/后续流程；非 401 只打印异常）。说明 Yamby 对 401 有专门处理，Lux 不应把权限错误返回 401。
- 404：`Y11/pd/C5043.java` 多处 `if (th.f20651 != 404) throw`，用于登录/服务器信息探测的回退（例如去掉 `/emby` 前缀重试）；不是所有 404 都静默。
- 结论：Yamby 的 401/404 处理比 Hills 丰富；Lux 侧最重要的是不要混用 401/403。

## 10. Lux 缺口清单（建议）

按“缺什么 / 哪个客户端 / 缺失时后果 / 建议优先级”列出，**只列建议，不含实现**。优先级：P1 影响核心播放或数据正确性；P2 影响主要页面功能；P3 影响次要功能；P4 仅体验/可接受。

| # | 缺口 | 客户端 | 缺失时的后果 | 证据 | 建议优先级 |
|---|---|---|---|---|---|
| 1 | `POST /Items/{id}/HideFromResume`（根路径）别名 | Hills（形态无法确认） | 若 Hills 用根路径：隐藏“继续观看”静默 404，列表不消失 | `HS11:14353`；Lux 仅有 `L/src/api/emby.rs:224` | **P1**（成本低，覆盖两种形态） |
| 2 | 独立查询参数 `IsResumable=true/false` 的解析 | Hills（无法确认） | 若 Hills 用独立参数：`/Items` 返回未过滤的完整列表，继续观看逻辑失效 | `HS11:25859`；Lux 只解析 `Filters`（`L/src/api/emby_catalog.rs:333-388`） | **P2**（先抓包确认再决定） |
| 3 | 根路径 `GET /Items/Resume`、`GET /Items/Latest` 别名 | Hills（推断用用户态） | 若走根路径：首页“继续观看/最新”空白 | `HS11:11616,23990`；Lux 只有用户态 | P2 |
| 4 | `GET /Users/{uid}/Items/Counts` | Hills（可能） | 若走用户态：各库计数 404/为空 | `HS11:23146`；Lux 只有根路径 | P3 |
| 5 | `GET /Items/{id}/Similar` | Hills、Yamby | 详情页“相似/推荐”为空（Yamby 静默） | `HS11:23739`；`Y11/pd/C5013.java:786`；`Y11/ac/C0165.java:252` | P2 |
| 6 | `GET /Items/{id}/AdditionalParts` 与 `GET /Videos/{id}/AdditionalParts` | Hills、Yamby | “额外片段/花絮”为空 | `HS11:26950`；`Y11/pd/C5013.java:141` | P3 |
| 7 | `GET /Items/{id}/Seasons`（若客户端用根 Items 形态） | Hills（无法确认） | 季列表 404，剧集页为空 | `HS11:25097`；Lux 只有 `/Shows/{id}/Seasons` | P3 |
| 8 | `GET /Shows/Upcoming` | Yamby | “即将播出”视图请求失败/空白 | `Y11/pd/C5066.java:77` | P3 |
| 9 | WebSocket `/embywebsocket`（至少明确 404 而不是 200 HTML；更好是握手） | Yamby | 实时会话/任务面板失效；当前实现会无退避重连，造成请求风暴 | `Y11/mc/C4000.java:91`；`Y11/r8/C5326.java:543-580`；Lux 无路由 | **P2**（先避免 200 HTML + 文档说明；实现握手可后置） |
| 10 | `Trickplay` 字段 / 缩略图集合端点 | Hills | 拖动条预览图缺失（Emby 协议走 ThumbnailSet；Jellyfin 协议走 Trickplay） | `H11/oi/f.java:119-140`；`H11/oi/t.java:1216-1260` | P4 |
| 11 | `Chapters[].ImageTag` | Hills | 均匀章节会被 `isInvalidChapters()` 判无效，跳过片头可能不显示 | `H11/com/mountains/player/models/Item.java:637-669`；Lux 的 `L/src/api/emby_catalog.rs:4908-4924` 无 `ImageTag` | P3（若项目需要片头跳过） |
| 12 | `MediaSource.ETag` | Hills（仅自拼直链时用） | 自拼 URL 带 `Tag=null`/裸 `Tag`；Lux 忽略该参数，无实际影响 | `H8/ji/r.java:822`；`H11/androidx/lifecycle/s0.java:252` | P4（无需实现，保持忽略） |
| 13 | `AlternateMediaSources`、`TagItems` | Hills（Dart 用途无法确认） | 可能缺少“其他版本/标签”展示；不影响播放 | `HS11:12760,17590` | P4（先确认 UI 用途） |
| 14 | `GET /System/Ext/ServerDomains` | Hills | 多线路/故障切换探测失败；按推断只是“不支持”，不影响主地址 | `HS11:7062`；SQLite 路由列 | P4（保持 404，勿 500/超时） |
| 15 | `MediaSourceCount` | 无（两个客户端都不读） | 无影响 | `Y11/oc/C4742.java:43-50` 无该键；Hills 无该串 | 不需要实现 |

## 11. 无法确认清单

1. **Hills `Items/Resume`、`Items/Latest` 是否真的带 `/Users/{uid}` 前缀**：Dart AOT 无模板字符串，只有 `/Users/` 与 `/Items/...` 片段；推断为中高，但不能算代码确认。
2. **Hills `HideFromResume` 的实际路径形态**：只有 `/HideFromResume` 片段；旧报告的根路径说法与 `Resume` 的根路径说法互相矛盾。
3. **Hills `IsResumable` 是 `Filters` 值还是独立参数**：只有孤立字符串。
4. **Hills 详情页的并发顺序与 404/空响应策略**：全部在 Dart；Java 层没有 Similar/AdditionalParts/Seasons/NextUp 的调用点。
5. **Hills 的 401/403 全局处理（是否登出）**：Java 层没有；Dart 层的 `Unauthorized` 字符串与 Supabase/Trakt/Simkl 混在一起，无法归因。
6. **Hills `ServerDomains` 404 的具体分支**：只有 `supportServerRouting`/`notSupportServerRouting` 字符串与本地 SQL 列；无法证明 404 触发哪条路径。
7. **Hills 缩略图集合（`ThumbnailSet`）的 URL**：`ni/a.java:83` 只有方法签名；调用方 `mi/d0.invokeSuspend` 方法体被 jadx 跳过。
8. **Hills `AlternateMediaSources`/`TagItems` 的 UI 用途**：Kotlin 模型没有字段，Dart 侧不可读。
9. **Yamby 未使用的 `/Audio/{id}/stream` 是否会在未来版本启用**：当前调用图不可达，但构造器保留。
10. **Yamby `Upcoming` 失败后的最终 UI**：`mo8016` 无本地 catch，异常交给分页基类，基类行为未继续下钻。

## 12. 对既有报告的修正与补充

| 旧说法 | 本次核对 | 证据 |
|---|---|---|
| Hills `MediaSource` 字段集 15 个 | 1.11.0 是 **17 个**（新增 `DisplayTitle`、`AddApiKeyToDirectStreamUrl`）；1.8.0 才是 15 个 | `H11/MediaSourceJsonAdapter.java:32`；`H8/…:35` |
| “`PlayMethod` 恒 `DirectStream`” | 结论对，但机制不是调用方显式传值，而是合成构造器的默认值；`PlaybackBody` 工厂传的是 null | `H11/PlaybackBody.java:349`；`H11/qi/v0.java:41` |
| “Hills 会话上报 `Content-Type` 视情况 json/text” | 更准确：Emby 协议先发 `text/plain`，**415 或 500+特定文案后才改 JSON 并记忆**；Jellyfin/fnos 直接 JSON | `H11/mg/g.java:195-230` |
| “Hills 1.8.0 会话 URL `/emby/...`” | 对，且是**硬编码** `/emby`；Jellyfin 模式下会打错前缀，1.11.0 才修 | `H8/ii/r.java:94`；`H11/androidx/lifecycle/k0.java:633` |
| “Hills 不传 `UserId` 查询参数” | 只对 1.8.0 成立。1.11.0 的播放 API 客户端由 `ni/f` 拦截器给每个同源请求追加 `UserId`，因此 PlaybackInfo / 会话 / 详情实际都带 `UserId` | `H11/ni/f.java:50`；`H11/ni/v.java:93-95`；1.8.0 `H8/ii/f.java` 无此逻辑 |
| “Hills `Items/Resume` 是根路径” | 1.8.0 的字符串组合更像 `/Users/{uid}/Items/Resume`；`Items/Counts` 才有完整根路径字面量 | `HS8:11019,25020` |
| “Yamby 用 `/Audio/{id}/stream` 播音频” | 构造器存在，但 2.1.0.11/2.0.5.5 的调用掩码全部包含 bit 32，音频分支不可达 | `Y11/EmbyPlaybackInfo.java:95`；`Y11` 全部 `m4315` 调用；`Y8` 全部 `m3751` 调用 |
| “Yamby 读 `MediaSourceCount`” | 不读；2.1.0.11 和 2.0.5.5 都没有该键 | `Y11/oc/C4742.java:43-50`；两版 grep 无结果 |
| “Yamby 用 `DefaultSubtitleStreamIndex` 选默认字幕” | 解析但从未读取；默认轨由 Yamby 自己的 `IsDefault` 逻辑决定 | `Y11/oc/C4742.java:48-49,166-167`；`Y11/EmbyMediaSources.java` |
| 待核对项“Hills 会请求 `Upcoming`” | 不请求；两版字符串都没有 `Upcoming` | `HS8`/`HS11` grep 无结果 |
| “`/System/Ext/ServerDomains` 404 可接受（代码确认）” | 只有字符串/本地列；404 分支在 Dart，**无法代码确认**，只能推断 | `HS11:7062`；无 Java 引用 |

## 13. 证据索引（关键文件）

Hills 1.11.0：

- `H11/ni/a.java`：Retrofit 风格接口（第 36、39、42、49、77、80、83、86 行）。
- `H11/ni/k.java`：第 74 行 `/Shows/{seriesId}/Episodes`；第 106 行 `/Users/{userId}/Items/{itemId}` + `X-Cache-Mode`。
- `H11/ni/l.java`：第 88 行 PlaybackInfo URL；第 100 行参数；第 104-113 行去重。
- `H11/ni/m.java`：第 59-65 行空 MediaSources 重试。
- `H11/ei/s0.java`：第 39-60 行令牌；第 66-70 行协议前缀；第 100-120 行图片 URL。
- `H11/androidx/lifecycle/s0.java`：第 187-260 行直链决策；第 237 行自拼 URL；第 248-252 行参数。
- `H11/xe/y.java` + `H11/xo/b.java`：URL builder 的 null 查询值渲染。
- `H11/mg/g.java`：第 164-280 行 Content-Type 回退。
- `H11/androidx/lifecycle/k0.java`：第 633 行会话 URL。
- `H11/com/mountains/player/models/`：`MediaSourceJsonAdapter.java:32`、`PlaybackBodyJsonAdapter.java:33,511-557`、`PlaybackBody.java:349`、`EventName.java`、`MediaSource.java:444`、`ItemJsonAdapter.java:45`、`ChapterJsonAdapter.java:32`、`JellyfinTrickplayInfoJsonAdapter.java:28`、`MarkerType.java`。
- `H11/qi/v0.java:41`、`H11/qi/u0.java:64-127`、`H11/qi/l1.java:141-170`、`H11/oi/f.java:119-140`、`H11/oi/t.java:563,1216-1260`、`H11/oi/v.java:238-240`、`H11/ni/f.java:50,54`、`H11/ni/i.java:142`、`H11/sp/r.java`、`H11/qi/a0.java:45`、`H11/qi/d0.java:59`、`H11/f1/j4.java:56`。
- `HS11`：7062（ServerDomains）、7801（`/Users/`）、11616（`/Items/Resume`）、14353（`/HideFromResume`）、23146（`/Items/Counts`）、23739（`/Similar`）、23990（`/Items/Latest`）、26950（`/AdditionalParts`）、12760/17590（Fields 全集）、22667（`Unauthorized`）。

Hills 1.8.0：

- `H8/ji/r.java:809-825`（直链决策，822 自拼）；`H8/ii/a.java:39,42,48,51,85`；`H8/ii/r.java:94-96`（会话 URL/Content-Type）；`H8/li/m0.java:40`（PlaybackBody 工厂）；`H8/com/mountains/player/models/MediaSourceJsonAdapter.java:35`；`H8/com/mountains/player/models/PreviewThumbnailData.java:160`（Jellyfin Trickplay 图片 URL）。
- `HS8`：11019、13647、18040、22764、24520、25020、25545。

Yamby 2.1.0.11：

- `Y11/mc/C4000.java:86-107`（WebSocket URL/连接/关闭）。
- `Y11/wc/C6328.java:70-146`（连接与帧）。
- `Y11/r8/C5326.java:543-580`（onClosed/onFailure 立即重连）。
- `Y11/a0/C0004.java:241-253,1377-1420`（开局消息、空 onFailure、消息过滤）。
- `Y11/pd/C5013.java:141,786`（AdditionalParts、Similar）。
- `Y11/pd/C5066.java:77`（Upcoming）。
- `Y11/pd/C5025.java:559`（401 分支）。
- `Y11/pd/C5043.java`（404 回退）。
- `Y11/com/hush/yamby/data/bean/EmbyPlaybackInfo.java:95,129`（Audio/video URL）。
- `Y11/oc/C4742.java:43-50,166-167`（字段集、默认索引）。
- `Y11/com/hush/yamby/data/bean/EmbyMediaSources.java:147-150`（toString）。
- `Y8/…`：2.0.5.5 的 `m3751` 调用掩码与 `MediaSourceCount` 负证据。

Lux：

- `L/src/api/emby.rs`：路由注册（第 47-244 行）。
- `L/src/api/routes.rs`：第 136-152 行根路径 + `/emby` 双挂载、大小写不敏感。
- `L/src/api/emby_catalog.rs`：第 13-99 行 `EmbyItemsQuery`；第 333-388 行 `Filters` 解析；第 1249-1289 行 Resume；第 1291-1310 行 Latest；第 2094-2148 行用户态/根 Items；第 4591-4770 行 MediaSource JSON；第 4908-4924 行 Chapter JSON。
- `L/src/api/playback.rs`：第 47 行 PlaybackInfo；第 1411-1452 行会话解析；第 290-430 行转码/直链 DTO。
- `L/src/application/images.rs:2934-2945`：`Thumbnail`→`THUMB` 映射。
