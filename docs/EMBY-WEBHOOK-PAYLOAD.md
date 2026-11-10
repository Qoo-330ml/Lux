# Emby Webhook 载荷字段参考

本文记录 Emby 4.9.1.90 内置 Webhooks 插件（1.0.39.0）实际发出的 JSON 字段结构，供 Lux 的 Emby 风格
Webhook 适配对照。只记录键名和类型，不含任何真实值、服务器 ID、用户名、文件路径或 URL。

## 来源与方法

- 插件自带的 `GET /Webhooks/Sample` 返回测试事件（`system.webhooktest`）的标准结构，
  包括简单 JSON 和 multipart 两种请求类型，正文相同。
- 在测试服务器上临时把 Webhooks 目标指向本地抓包端，触发登录、播放开始和停止，记录请求正文。
  抓包结束后已删除原始正文，仅保留本文的结构描述。

## 请求格式

- 简单模式（`application/json; charset=utf-8`）：请求正文即 JSON 对象。
- multipart 模式：JSON 作为一个 `application/json` 部件发送，外层为 `multipart/form-data`。

## 通用顶层字段

所有已观察事件都包含：

| 字段 | 类型 | 说明 |
|---|---|---|
| `Title` | string | 人类可读标题，例如"用户已验证身份"、"开始播放" |
| `Description` | string | 人类可读描述，部分事件有 |
| `Date` | string | RFC 3339 时间，带 7 位小数 |
| `Event` | string | 事件名，见下表 |
| `Severity` | string | 例如 `Info` |
| `Server` | object | `{ Name, Id, Version }` |
| `User` | object | `{ Name, Id }`，事件关联用户时出现 |

## 已确认的事件

| `Event` | 触发来源 | 额外对象 |
|---|---|---|
| `user.authenticated` | 用户登录成功 | `Session` |
| `playback.start` | 开始播放 | `Item`、`PlaybackInfo`、`Session` |
| `playback.stop` | 停止播放 | `Item`、`PlaybackInfo`、`Session` |
| `system.webhooktest` | 发送测试通知（插件示例） | 无 |

`user.authenticated` 的 `Session` 键：`RemoteEndPoint`、`Client`、`DeviceName`、`DeviceId`、
`ApplicationVersion`、`Id`。

## `Item`（媒体项，播放事件）

顶层键：`Name`、`OriginalTitle`、`ServerId`、`Id`、`DateCreated`、`DateModified`、
`PresentationUniqueKey`、`Container`、`PremiereDate`、`ExternalUrls`（列表，元素 `Name`、`Url`）、
`Path`、`Overview`、`Taglines`（列表）、`Genres`（字符串列表）、`CommunityRating`、`RunTimeTicks`、
`Size`、`FileName`、`Bitrate`、`ProductionYear`、`ProviderIds`（`Imdb`、`Tvdb`、`Tmdb`）、
`IsFolder`、`ParentId`、`Type`、`Studios`（列表，元素 `Name`、`Id`）、`GenreItems`（同上）、
`PrimaryImageAspectRatio`、`MediaStreams`（列表）、`ImageTags`（`Primary`、`Logo`、`Banner`、`Disc`、`Thumb`）、
`BackdropImageTags`（字符串列表）、`Chapters`（列表，元素 `StartPositionTicks`、`Name`、`ImageTag`、
`MarkerType`、`ChapterIndex`）、`MediaType`。

`MediaStreams` 元素键（视频流示例）：`Codec`、`TimeBase`、`VideoRange`、`DisplayTitle`、`IsInterlaced`、
`BitRate`、`BitDepth`、`RefFrames`、`IsDefault`、`IsForced`、`IsHearingImpaired`、`Height`、`Width`、
`AverageFrameRate`、`RealFrameRate`、`Profile`、`Type`、`AspectRatio`、`Index`、`IsExternal`、
`IsTextSubtitleStream`、`SupportsExternalStream`、`Protocol`、`PixelFormat`、`Level`、
`ExtendedVideoType`、`ExtendedVideoSubType`、`ExtendedVideoSubTypeDescription`、`AttachmentSize`。

## `PlaybackInfo`（播放事件）

顶层键：`PositionTicks`、`PlaylistIndex`、`PlaylistLength`、`PlaySessionId`、`MediaSource`。
`playback.stop` 额外有 `PlayedToCompletion`。

`MediaSource` 键：`Id`、`ItemId`、`Name`、`Path`、`Type`、`Container`、`Protocol`、`Bitrate`、`Size`、
`RunTimeTicks`、`Formats`、`Chapters`、`MediaStreams`、`RequiredHttpHeaders`、`IsRemote`、
`IsInfiniteStream`、`HasMixedProtocols`、`ReadAtNativeFramerate`、`RequiresClosing`、`RequiresLooping`、
`RequiresOpening`、`SupportsDirectPlay`、`SupportsDirectStream`、`SupportsProbing`、`SupportsTranscoding`、
`AddApiKeyToDirectStreamUrl`。

## `Session`（播放事件）

键：`RemoteEndPoint`、`Client`、`DeviceName`、`DeviceId`、`ApplicationVersion`、`Id`。

## 与 Lux 当前 Emby 风格载荷的差距

以 `src/application/webhooks.rs` 中 `build_event_payload_for_format` 为准，当前 Lux 的 Emby 风格输出：

- 已有：`Event`、`Item.Id/Type/Name`、`Server.Id`、`ItemID`、`PlaySessionId`、`PositionTicks` 等。
- 缺少顶层 `Title`、`Description`、`Date`、`Severity`，这些是第三方联动最常读取的字段。
- 缺少 `User`（`Name`、`Id`）和 `Session` 对象，播放事件的"谁在看什么"无法还原。
- 缺少 `Server.Name`、`Server.Version`。
- `Item` 缺少 `Path`、`FileName`、`ProviderIds`、`MediaStreams`、`RunTimeTicks`、`ProductionYear` 等。
- 字段名存在自创形式（`ItemID`、`ServerID`、`ItemIndex`、`PlaybackState`），Emby 原生没有这些键。

`Path`、`FileName` 与 `User` 属于敏感信息，是否向外发送需要按 `CLAUDE.md` 的脱敏规则另行决定。

## 未覆盖的事件

本次未抓到以下事件，需要真实客户端或真实媒体变化才能触发，后续补充：

- `playback.pause`、`playback.unpause`、`playback.progress`：模拟的进度上报没有产生这些事件，需要真实播放器操作。
- `library.new`、`library.deleted`：需要真实新增或移除媒体。
- `item.markplayed`、`item.markunplayed`、`item.rate`：会改动用户数据，本次未触发。
- `system.*`（服务器启动、更新等）、`scan` 相关事件：未观察。

## 第二次抓取补充（2026-11-02，Emby 4.9.1.90，Webhooks 1.0.39.0）

以下内容由真实请求确认。用户名、设备名、IP、服务器名均以占位符表示，不记录真实值。

### `Title` / `Description` 文案模板（服务器语言为中文）

| `Event` | `Title` 模板 | `Description` 模板 |
|---|---|---|
| `system.serverstartup` | `Emby Server 于 {Server} 启动。` | 无 |
| `system.updateavailable` | `Emby Server {Version} 可用于 {Server}` | 无 |
| `scheduledtasks.completed` | `{Server} 上 {TaskName} 已完成` | `运行时间：{Duration}` |
| `user.authenticated` | `{User} 已在 {Server} 上被验证` | `{DeviceName} {RemoteIp}` + 空行 + `Time` + 空行 + 本地时间 |
| `playback.start` | `{User} 在 {DeviceName} 上开始播放 {ItemName}` | 空字符串 |
| `playback.stop` | `{DeviceName} 上 {User} 已停止播放 {ItemName}` | 空字符串 |
| `playback.pause` | `{DeviceName} 上 {User} 已暂停播放 {ItemName}` | `null` |
| `playback.unpause` | `{DeviceName} 上 {User} 已恢复播放 {ItemName}` | `null` |
| `system.webhooktest` | `Test Notification` | `Test Notification Description` |

`user.authenticated` 的 `Description` 中，`Time` 行的格式为 `2026年11月2日星期一 16:16`（服务器本地时间，中文星期）。

### `Date` 格式

RFC 3339 UTC，小数部分 7 位，例如 `2026-11-02T08:17:51.7612127Z`。Lux 的时间戳只到秒，
输出时补 `.0000000Z`。

### 字段键（已确认）

- `playback.start` / `playback.stop`：顶层 `Date`、`Event`、`Item`、`PlaybackInfo`、`Server`、`Session`、
  `Severity`、`Title`、`User`；`playback.stop` 的 `PlaybackInfo` 多 `PlayedToCompletion`。
- `Session`：`ApplicationVersion`、`Client`、`DeviceId`、`DeviceName`、`Id`、`RemoteEndPoint`。
- `PlaybackInfo`：`MediaSource`、`PlaySessionId`、`PlaylistIndex`、`PlaylistLength`、`PositionTicks`。
- `Item`（电影）：`Type` 为 `Movie`，`MediaType` 为 `Video`。
- `system.serverstartup` 与 `scheduledtasks.completed` 没有 `User` 键。
- `system.updateavailable` 多 `PackageVersionInfo` 键。

### 已知的抓取细节

- 同一事件在本次抓取中出现两次（两个相同请求），原因未确认，实现时需要核对投递是否重复。
- 暂停事件的 `/Sessions/Playing/Pause` 返回 404，进度上报里的 `IsPaused` 没有产生 `playback.pause`，
  因此暂停、继续事件仍未抓到。
- `library.new`、`library.deleted` 和扫描相关的文案仍未抓到，需要真实的媒体变化才能确认。

## 实现状态（2026-11-02）

已在 `src/application/webhooks.rs`（Emby 适配器）和 `src/api/playback.rs`（数据来源）中对齐：顶层 `Title`/`Description`/`Date`/`Severity`，
`Server.Name`/`Version`，`User`，`Session`（含所有播放事件的 `RemoteEndPoint`），`PlaybackInfo`，`Item` 的类型与年份、时长、外部 ID、简介。
播放开始和停止的 `Title` 使用上文的中文模板。Lux 原生格式未变。

已补充 `Item.Path` 与 `Item.FileName`：由库根（`library_roots.canonical_path`）与库内相对路径拼接，`.strm` 给出本地 `.strm` 文件路径，不给外部 URL。

已补充 `Item.MediaStreams` 与 `PlaybackInfo.MediaSource`：流详情复用 Emby API 的流 DTO，媒体源提供 `Container`、`Size`、`Bitrate`、`RunTimeTicks`、`Path`，不包含外部地址或签名 URL。

未完成：`Session.Id`，暂停/继续/进度文案，媒体库与扫描、系统事件文案。

## 文案来源（服务端资源，2026-11-02 核对）

文案来自 Emby 服务端的资源文件（`Emby.Resources.dll` 的默认资源键名与 `zh-CN` 资源值），不是抓包猜测。
键名与文案的对应关系：

- `UserAuthenticated` → `{0} 已在 {1} 上被验证`（已与抓包一致）
- `UserStartedPlayingItemWithValues` → `{0} 在 {2} 上开始播放 {1}`（已与抓包一致；片名本身若带引号，原样保留，不再额外加引号）
- `UserStoppedPlayingItemWithValues` → `{2} 上 {0} 已停止播放 {1}`（已与抓包一致）
- `UserPausedItemWithValues` → `{2} 上 {0} 已暂停播放 {1}`（已与抓包一致）
- `UserResumedItemWithValues` → `{2} 上 {0} 已恢复播放 {1}`（已与抓包一致；事件名 `playback.unpause`；Lux 目前没有独立的恢复事件，见下）
- `NumItemsAddedToGroupOnServerName` → `{2} 上已添加了 {0} 项到 {1}`（媒体新增，推断，未证实）
- `NumItemsRemovedFromGroupOnServerName` → `{2} 上已移除了 {1} 中的 {0} 项`（媒体移除，推断，未证实）

媒体库与扫描事件需要发布端补充库名与数量后才能使用这些文案，目前未实现。

## 第三次抓取（2026-11-02，真实播放：开始、暂停、恢复、拖动、停止）

- `playback.pause` 与 `playback.unpause` 的标题与资源模板一致，`Description` 为 `null`。
- 两者的键结构与 `playback.start` 相同，另有 `TranscodingInfo` 键（依赖转码状态，Lux 目前没有对应数据，未实现）。
- 每次事件在请求中出现 2 到 3 次，内容和 `Date` 完全相同，是 Emby 侧的重复发送。
- Lux 目前没有"从暂停恢复"的独立事件，所以 `playback.unpause` 尚未在 Lux 中发出。
