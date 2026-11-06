# Hills / Yamby 真机请求抓取（2026-11-06）

范围：只做真机与请求日志（方法 A/B）。解包静态分析由并行任务负责，本文只在解读抓包时引用少量已解包结论。
未改 `src/`、`tests/`、迁移和 `web/`，未实现任何缺口。

## 0. 结论摘要

- 两个客户端的首页“继续观看”“最新”都走 **带用户前缀** 的
  `/emby/Users/<userId>/Items/Resume` 与 `/emby/Users/<userId>/Items/Latest`；
  抓包中没有出现不带用户前缀的 `/emby/Items/Resume`、`/emby/Items/Latest`。
- 在已覆盖的首页、媒体库“继续观看”、详情和播放流程里，**没有**任何请求带
  `Filters=IsResumable`。Hills 的 `libapp` 里存在该字符串，但它不由这些流程触发。
- `HideFromResume` 是 **带 `/Users/<userId>` 的 POST**，Hills 实抓为
  `/emby/Users/<userId>/Items/<itemId>/HideFromResume?Hide=true`，Lux 返回 200。
- 详情页真实会打 `/Items/<id>/Similar`（Hills 与 Yamby 都打，Lux 返回 **404 空响应**）；
  页面本身继续渲染，没有可见报错弹窗，相关推荐区直接缺失。
- Hills 详情请求的 `Fields=` 实际是 `ProviderIds,ExternalUrls,Trickplay`，**不含**
  `AlternateMediaSources`、`MediaSources`、`MediaSourceCount`；多版本信息来自
  `PlaybackInfo`，不是详情 `Fields`。
- 两个客户端起播时都直接使用 `PlaybackInfo` 里的 **`DirectStreamUrl`**（URL 带 Lux 的
  `luxPlaybackSignature` 等签名参数），**没有 `Tag=` 参数**；因此
  `MediaSources[].ETag` 缺失对这两个客户端的起播没有影响。
- `Sessions/Playing`、`/Progress`、`/Stopped` 的请求体字段集合已抓到；Yamby 在本次抓包中
  **没有**发起 `/embywebsocket`，Lux 对该路径（普通 GET 和 Upgrade 握手）都返回 **404 空响应**。

## 1. 方法与边界

### 1.1 设备与版本

| 项 | 值 |
|---|---|
| 设备 | `271f28a2`，Xiaomi `23127PN0CC`（houji），Android 17 |
| Hills | `com.mountains.hills` 1.11.0（versionCode 4139） |
| Yamby | `com.hush.yamby` 2.1.0.11（versionCode 20010012） |
| adb | `$HOME/Library/Android/sdk/platform-tools/adb`（`客户端解包/04-脚本/env.sh` 的 `$ADB`） |

手机里原有的生产连接是 `https://<host>`。**没有**改、重启或部署该服务，也没有把它的
日志当证据；生产连接只做了正常读取（打开列表/首页），没有保存任何编辑。

### 1.2 可控实例与抓包通道

手机连不到本机时无法读生产服务器日志，因此在 worktree 内起了一个**独立**的 Lux 实例：

- 本分支 `cargo build --locked` 的 debug 二进制，版本 `0.7.14`。
- `LUX_CONFIG_DIR=.tmp/lux/config`、SQLite、`LUX_HTTP_ADDR=0.0.0.0:8097`。
- 媒体库根目录 `.tmp/lux/media`，测试素材用 ffmpeg 生成：
  - `Test Movie (2020) - 1080p.mp4` 与 `Test Movie (2020) - 720p.mp4`（同一影片两个版本）
  - `Test Show/Season 01/Test Show S01E01.mp4`
  - `poster.jpg` / `fanart.jpg`
- 扫描结果：影片 2 个 `MediaSources`，剧集 1 集；详情/播放均可用。

在 Lux 前面加了一个仅本机使用的日志反向代理（`0.0.0.0:8098 -> 127.0.0.1:8097`），
逐条记录 method、path、脱敏后的 query、少量客户端头和请求体（上限 16 KiB）。
`Authorization`、`X-Emby-Token`、`api_key` 等在写入日志前替换为 `<redacted>`。
所有抓包原始文件都在 `.tmp/capture/`（已被忽略，不提交）。

### 1.3 手机上的临时改动与恢复

| 操作 | 状态 |
|---|---|
| Hills 新增临时服务器 `http://<lan-host>:8098`（显示名 `<testServer>`） | 已删除，服务器列表恢复为原两个 |
| Yamby 新增临时服务器（同一地址） | 已删除，服务器列表恢复为原一个 |
| 登录临时测试账号 | 只存在于本地测试实例，实例数据已删除 |
| 临时把 `accelerometer_rotation` 从 1 改为 0 以保持竖屏 | 已恢复为 1；`user_rotation` 始终为 0 |
| 播放测试影片、暂停/停止、从“继续观看”移除 | 仅作用于本地测试实例 |
| 结束状态 | 已锁屏（`mWakefulness=Dozing`），与开始时一致 |

未 root，未安装证书，未设置设备级代理，未清除应用数据，未卸载/重装，未删除任何原有连接。
为了在本地实例制造“继续观看”卡片，曾在**测试库**里把 `resume_min_ticks` 调为 0 并写入测试
用户的播放位置；该数据库随后整体删除，与生产无关。

### 1.4 局限

- 没有做设备级代理/MITM；Flutter 默认不走系统代理，装证书需要改手机，按任务约定未做。
- 代理不是 WebSocket 隧道，无法完成 `101` 后的双向转发；但握手请求本身会被记录（本次没有记录到）。
- 测试影片是单文件，没有构造 CD1/CD2 多部分；`AdditionalParts` 没有被触发。
- `Shows/Upcoming`、`Items/<id>/Seasons`、`Localization/cultures` 没有在已走的 UI 路径里出现，
  只能给出 Lux 的直接探测状态，不能断言客户端在所有界面都不发。
- 开始抓包前改过一次测试账号密码，导致两个客户端已有的测试令牌失效；重登后继续。只影响本地实例。

## 2. 问题 1–7

### 问题 1：首页“继续观看”“最新”用哪种路径

| 客户端 | 实抓请求（脱敏） | 状态 | 结论 |
|---|---|---|---|
| Hills | `GET /emby/Users/<userId>/Items/Resume?Fields=ProductionYear,EndDate,Status,ProviderIds,CanDelete,Path,CanDownload&ImageTypeLimit=1&MediaTypes=Video&ParentId=<libraryId>&Limit=60&StartIndex=0&Recursive=...` | 200 | 带 `/Users/{id}` |
| Hills | `GET /emby/Users/<userId>/Items/Latest?ParentId=<libraryId>&limit=20&ImageTypeLimit=1&EnableImageTypes=Primary,Backdrop,Thumb,Logo&Fields=...` | 200 | 带 `/Users/{id}` |
| Yamby | `GET /emby/Users/<userId>/Items/Resume?EnableImageTypes=Primary,Backdrop,Thumb&Fields=PrimaryImageAspectRatio,BasicSyncInfo,ProductionYear,OfficialRating,CanDelete,CanDownload&MediaTypes=Video&Limit=30&StartIndex=0&Recursive=true&ParentId=` | 200 | 带 `/Users/{id}` |
| Yamby | `GET /emby/Users/<userId>/Items/Latest?EnableImageTypes=...&Fields=...&Limit=...` | 200 | 带 `/Users/{id}` |

补充证据：

- 不带用户前缀的 `/emby/Items/Resume`、`/emby/Items/Latest` 在全部抓包中出现 **0 次**。
- `Filters=IsResumable` 在全部抓包中出现 **0 次**；Hills 媒体库的“继续观看”标签页仍然调用
  `/Users/<userId>/Items/Resume`。
- Hills 首页的 Resume 请求 `ParentId=` 为空；进入某个媒体库后 `ParentId=<libraryId>`。
- Hills 的 `libapp` 里确实有 `IsResumable` 字符串，但本次覆盖的流程没有使用它。

**结论：确认会发（带用户前缀）**。`IsResumable` **确认在这些流程中不发**；不能排除其它未进入的
筛选/搜索界面会用到它。

置信度：路径 **高**；`IsResumable` **中**。

### 问题 2：`HideFromResume` 的完整路径

| 客户端 | 实抓请求 | 状态 | 结论 |
|---|---|---|---|
| Hills | `POST /emby/Users/<userId>/Items/<itemId>/HideFromResume?Hide=true` | 200 | 带 `/Users/{id}` |
| Yamby | 解包代码为同一形态（`/emby/Users/<userId>/Items/<itemId>/HideFromResume?Hide=true`）；本次未在 UI 找到该入口，未实抓 | — | 静态一致 |

触发方式：Hills 首页“继续观看”卡片长按 → “从继续观看中移除”。请求成功后该卡片从首页消失。
Lux 的路由正好是 `/Users/{user_id}/Items/{item_id}/HideFromResume`，与客户端一致。

**结论：确认会发，且是带 `/Users/{id}` 的 POST**。Hills 置信度高；Yamby 为静态高置信、动态未确认。

### 问题 3：详情页会请求哪些端点、Lux 返回什么

| 端点 | Hills 是否请求 | Yamby 是否请求 | Lux 状态 | 客户端可见行为 |
|---|---|---|---|---|
| `/Items/<id>/Similar` | 是（影片、剧集详情各一次） | 是（影片、剧集各一次） | **404 空响应** | 详情页正常渲染，无弹窗/Toast；“相似/更多”区缺失 |
| `/Shows/Upcoming` | 未观察到 | 未观察到（代码里有，未触发） | **404 空响应** | 未验证 |
| `/Items/<id>/AdditionalParts` | 未观察到 | 未观察到（代码里是 `/Videos/<id>/AdditionalParts`，单文件影片未触发） | **404 空响应** | 未验证 |
| `/Items/<id>/Seasons` | 未观察到 | 未观察到 | **404 空响应** | — |
| `/Shows/<id>/Seasons` | 是 | 是 | **200** | 正常 |
| `/Tags` | 是（媒体库筛选面板加载时） | 未观察到 | **404 空响应** | 筛选面板仍能打开，标签列表为空；截图中未见报错 |
| `/OfficialRatings` | 是（同上） | 未观察到 | **404 空响应** | 评级列表为空 |
| `/Localization/cultures` | 未观察到 | 未观察到 | **404 空响应** | — |

实抓样例：

- Hills 影片详情：
  `GET /emby/Items/<itemId>/Similar?UserId=<userId>&Fields=ProductionYear,EndDate,Status,CommunityRating,PrimaryImageAspectRatio,RecursiveItemCount,ProviderIds,CanDelete,Path,CanDownload&Limit=20&ImageTypeLimit=1...` → 404
- Yamby 影片详情：
  `GET /emby/Items/<itemId>/Similar?Limit=10&UserId=<userId>&ImageTypeLimit=1&Fields=BasicSyncInfo,...` → 404
- Hills 剧集详情：`/emby/Shows/<seriesId>/Seasons?...` → 200，
  `/emby/Shows/NextUp?...` → 200，`/emby/Shows/<seriesId>/Episodes?...` → 200，
  `/emby/Items/<seriesId>/Similar?...` → 404
- Yamby 剧集详情：`/emby/Shows/<seriesId>/Seasons` → 200、`/emby/Shows/NextUp` → 200、
  `/emby/Users/<userId>/Items?ParentId=<seriesId>&Filters=IsNotFolder...` → 200、
  `/emby/Items/<seriesId>/Similar` → 404。
- Yamby 还会对 **Season 条目** 调 `POST /emby/Items/<seasonId>/PlaybackInfo`，Lux 返回 **404**
  （不是缺路由，而是该条目不可播放）；UI 未崩溃。

额外发现（不在 7 问内，但和同一批详情/筛选有关）：

- Hills 播放页请求 `GET /emby/Items/<itemId>/ThumbnailSet?MediaSourceId=...&Width=720` → **404**。
- Hills 媒体库筛选面板加载 `Genres`/`Years`/`Studios` 时带了 **空的 `Limit=`**，Lux 返回 **400**
  （`Failed to deserialize query string: Limit: cannot parse integer from empty string`）；
  `Tags`/`OfficialRatings` 因为根本没有路由而是 404。把 `Limit=` 去掉后 `Genres` 返回 200。
  也就是说筛选面板一部分是“空列表”，一部分是“400”，不是同一种失败。

**结论：`Similar` 是确认的真实缺口（两个客户端每次详情都打，404）；`Tags`、`OfficialRatings`
是 Hills 筛选面板的真实缺口（404）；`Shows/Upcoming`、`AdditionalParts`、`Items/<id>/Seasons`、
`Localization/cultures` 本次无法确认客户端一定会发，但 Lux 目前一律 404。**

置信度：`Similar`、`Tags`、`OfficialRatings`、`Shows/<id>/Seasons` **高**；其余“未观察到”的
端点 **低/无法确认**。

### 问题 4：详情 `Fields=` 实际取值与多版本表现

Hills 详情请求（`/Users/<userId>/Items/<itemId>`）的 query 原样是：

```
EnableImageTypes=Primary,Backdrop,Thumb,Logo
&ImageTypeLimit=1
&Fields=ProviderIds,ExternalUrls,Trickplay
```

列表类请求里出现过的 `Fields`（去重）：

```
ProductionYear,EndDate,Status,ProviderIds,CanDelete,Path,CanDownload
ProductionYear,EndDate,Status,PrimaryImageAspectRatio,CommunityRating,RecursiveItemCount,ProviderIds,CanDelete,Path,CanDownload
...同上...,Trickplay
ProductionYear,EndDate,Status,CommunityRating,PrimaryImageAspectRatio,RecursiveItemCount,ProviderIds,CanDelete,Path,CanDownload
ProductionYear,PrimaryImageAspectRatio,CommunityRating,Overview,OfficialRating,Genres,ProviderIds,CanDownload
PrimaryImageAspectRatio,RecursiveItemCount,ProviderIds,ExternalUrls,CanDelete,Path
ProductionYear,Width,Height,ProviderIds,Overview,ParentId,People,CommunityRating,Chapters,CanDelete,Path,CanDownload,DateCreated
ProductionYear,Overview,Chapters,Width,Height,ProviderIds,ParentId,People,CommunityRating,CanDelete,Path,CanDownload,DateCreated,Trickplay
ParentId
```

- **没有任何请求包含 `AlternateMediaSources`**；`MediaSources`、`MediaSourceCount` 也没有出现在
  客户端请求的 `Fields` 里。`Trickplay` 在详情和列表里都出现。
- Hills 的 `libapp` 里存在 `,MediaSources,AlternateMediaSources` 这样的后缀字符串，但本次覆盖的
  流程没有把它拼进请求；不能据此认为客户端在详情页依赖它。
- Yamby 详情请求 `/Users/<userId>/Items/<itemId>` **没有 Fields 参数**；列表 Fields 也不含
  `AlternateMediaSources`。

多版本实测（同一影片两个文件，Lux 返回 2 个 `MediaSources`）：

- Hills：详情页只看到默认的 `... - 1080p.mp4` 一行；`POST /Items/<id>/PlaybackInfo?IsPlayback=true`
  返回 200，Lux 的响应里本来就有两个 source。未在 Hills UI 找到切换非默认版本的入口。
- Yamby：详情页显示“版本 SD • 1.75 MB”，点开后列出
  `... - 1080p.mp4` 与 `... - 720p.mp4`，选择 1080p 后正常起播。
  该版本列表来自 `PlaybackInfo`（Yamby 先发 `IsPlayback=false` 的 PlaybackInfo）。

**结论：详情 `Fields` 不含 `AlternateMediaSources`/`Trickplay` 之外的媒体源字段；
`AlternateMediaSources`、`MediaSourceCount` 缺失不是这两个客户端的阻塞点，它们用
`PlaybackInfo` 取版本。Hills 是否能选择非默认版本未能确认。**

置信度：**高**（Fields 与 Yamby 版本列表）；Hills 版本切换入口 **无法确认**。

### 问题 5：起播用的是 `DirectStreamUrl` 还是自拼 URL，`Tag=` 是什么

两个客户端实抓到的起播请求（Hills 与 Yamby 形态一致）：

```
GET /emby/Videos/<itemId>/stream.mp4
  ?MediaSourceId=<sourceId>
  &DeviceId=<deviceId>
  &PlaySessionId=<playSessionId>
  &static=true
  &UserId=<username>
  &luxPlaybackUserId=<userId>
  &luxPlaybackAdmin=1
  &luxPlaybackTicketVersion=2
  &luxPlaybackExpires=<epoch>
  &luxPlaybackSignature=<signature>
  &api_key=<token>
```

状态：Hills 206；Yamby 206（两次 Range 请求）。

判据：

- `luxPlaybackSignature`、`luxPlaybackExpires`、`luxPlaybackTicketVersion`、`luxPlaybackUserId`
  只可能来自 Lux 自己生成的 `DirectStreamUrl`；客户端自拼 URL 不会带这些参数。
- `static=true` 是小写，和 Lux `PlaybackInfo` 里的 URL 形态一致；Hills 解包报告里描述的
  自拼形态是 `Static=true&...&Tag={ETag}`，本次没有出现。
- **两个客户端的 URL 都没有 `Tag=` 参数**。`api_key` 是客户端因为
  `AddApiKeyToDirectStreamUrl=true` 追加的。

**结论：确认两个客户端都使用 `PlaybackInfo.DirectStreamUrl`，没有自己拼 `Tag=`。
`MediaSources[].ETag` 缺失不影响这两个客户端的起播；即使客户端回退到自拼 URL，Lux 的
stream 路由也不校验 `Tag`。**

置信度：**高**。

### 问题 6：`Sessions/Playing`、`/Progress`、`/Stopped` 字段与 WebSocket

#### Hills

`POST /emby/Sessions/Playing?reqformat=json&UserId=<userId>` → 204
`POST /emby/Sessions/Playing/Progress` → 204
`POST /emby/Sessions/Playing/Stopped` → 204

请求体字段集合（脱敏后按出现顺序）：

```
VolumeLevel, IsMuted, IsPaused, RepeatMode, SubtitleOffset, PlaybackRate,
MaxStreamingBitrate, PositionTicks, PlaybackStartTimeTicks, SubtitleStreamIndex,
AudioStreamIndex, PlayMethod, PlaySessionId, MediaSourceId, CanSeek, ItemId,
PlaylistIndex, PlaylistLength, Shuffle
```

- `Progress` 额外带 `EventName`，实测出现 `"TimeUpdate"` 和 `"Pause"`。
- 起播/停止的 body **没有** `EventName`。
- 实测值：`PlayMethod="DirectStream"`、`RepeatMode="RepeatNone"`、`PlaybackRate=1.4`、
  起播 `PositionTicks=0`、`AudioStreamIndex` 从 `-1` 变为 `0`、`SubtitleStreamIndex=-1`、
  `CanSeek=true`、`PlaylistIndex=0`、`PlaylistLength=1`、`Shuffle=false`。
- 没有 `NowPlayingQueue`，没有 `UserId` 字段（UserId 在 query）。

#### Yamby

`POST /emby/Sessions/Playing` → 204
`POST /emby/Sessions/Playing/Stopped` → 204
（本次播放很快结束，**没有抓到 `/Progress`**）

起播 body 字段集合：

```
VolumeLevel, NowPlayingQueue, IsMuted, IsPaused, MaxStreamingBitrate, RepeatMode,
PlaybackStartTimeTicks, SubtitleOffset, PlaybackRate, PositionTicks,
SubtitleStreamIndex, AudioStreamIndex, PlayMethod, PlaySessionId, MediaSourceId,
PlaylistIndex, PlaylistLength, CanSeek, ItemId
```

- 与 Hills 的差异：Yamby 有 `NowPlayingQueue`（实测空数组），**没有 `Shuffle`**；
  起播/停止均无 `EventName`。
- 实测值：`PlayMethod="DirectStream"`、`RepeatMode="RepeatNone"`、`PlaybackRate=1.0`、
  `AudioStreamIndex=1`、`SubtitleStreamIndex=1`（客户端自选默认值）。
- 停止 body 的 `PositionTicks` 为 390000000（继承上一次的 39 秒位置），不是 0。

#### WebSocket

- 全部抓包中 `/embywebsocket` 请求数：**0**。Yamby 没有在该次会话中发起握手，
  因此不能从动态上确认它“会不会连”。
- 直接探测 Lux：`GET /embywebsocket?api_key=<token>&deviceId=...` 返回 **404 空响应**；
  带 `Connection: Upgrade`、`Upgrade: websocket`、`Sec-WebSocket-Key` 的握手请求同样返回
  **404 空响应**。Lux 目前没有 WebSocket 路由。

**结论：请求体字段集合确认（Hills 三类齐全，Yamby 起播/停止齐全）；Yamby 的
`/embywebsocket` 行为无法确认，若连接则拿到 404。**

置信度：请求体 **高**；WebSocket **低（未观察到）**。

### 问题 7：图片请求的 `maxWidth/quality/tag` 实际取值

| 客户端 | 实抓参数 | 说明 |
|---|---|---|
| Hills | `tag=<uuid>&quality=90&api_key=<redacted>&maxWidth=<n>&maxHeight=<n>` | 尺寸成对出现，按 100 向上取整；也有一部分只带 `tag`/`quality`、不带尺寸 |
| Yamby | `maxWidth=<n>&tag=<uuid>&quality=90` | **只带 maxWidth，没有 maxHeight**；不追加 api_key（走认证头） |

Hills 实测的 `(maxWidth, maxHeight)`：

```
(400, 600)   Primary
(600, 300)   Primary
(1200, 1100) Primary / Backdrop
(1200, 1200) Backdrop
(1200, 1800) Primary
(2700, 900)  Backdrop
(600, 400)   Backdrop
(2700, -)    Backdrop（只有 maxWidth）
(400, -)     Primary（只有 maxWidth）
```

Yamby 实测的 `maxWidth`：`400`、`560`、`720`、`1080`、`1920`；
全部请求都带 `tag=<uuid>&quality=90`。

**结论：`quality` 固定 90；`tag` 始终存在；Hills 同时给 maxWidth/maxHeight，
Yamby 只给 maxWidth。** 置信度 **高**。

## 3. 对 Lux 的直接影响判断

### 确认的真实缺口（有客户端实抓 404）

1. `/Items/<id>/Similar`：两个客户端每次进入影片/剧集详情都会请求，Lux 404。
   当前表现是“没有相似内容”，没有崩溃。
2. `/Tags`、`/OfficialRatings`：Hills 打开媒体库筛选面板时请求，Lux 404，筛选列表为空。
3. `/Items/<id>/AdditionalParts`、`/Shows/Upcoming`、`/Items/<id>/Seasons`、
   `/Localization/cultures`：Lux 一律 404；本次没有抓到客户端请求，属于“可能缺口”，
   优先级应低于上面三项。

### 确认不是缺口

- `MediaSources[].ETag`：两个客户端都用 `DirectStreamUrl`，不带 `Tag`，起播 206 正常。
- 详情 `Fields` 不含 `AlternateMediaSources`/`MediaSourceCount`：客户端用 `PlaybackInfo` 取版本。

### 健壮性问题（不是缺路由）

- 客户端会给 `Genres`/`Years`/`Studios` 发空的 `Limit=`，Lux 返回 400；
  这是客户端能观察到的失败（筛选面板空），不是 404。
- Season 条目上的 `PlaybackInfo` 返回 404；客户端没有崩溃，但这是非预期交互。

## 4. 建议的后续 LUX 任务（只列建议，不实现）

1. 实现 `/Items/{id}/Similar`（Movie/Series），接受 `UserId`、`Fields`、`Limit`，返回
   `Items`/`TotalRecordCount` 信封；先覆盖“同类型 + 同库 + 排除自身”的简单推荐。
2. 实现 `/Tags` 与 `/OfficialRatings` 的媒体库筛选元数据（接受 `UserId`、`Recursive`、
   `ParentId`、排序参数），返回 `Items` 列表；两者当前是 404。
3. 让 `Genres`/`Years`/`Studios` 接受空 `Limit=`/`StartIndex=`（按未传处理），避免客户端 400。
4. 增加 `/Items/{id}/Seasons` 作为 `/Shows/{id}/Seasons` 的别名，兼容不同客户端。
5. 评估 `/Shows/Upcoming`（日历）与 `/Items/{id}/AdditionalParts`（多部分/花絮）是否要实现；
   先确认客户端是否只在特定入口调用。
6. 增加 `/Localization/cultures` 的静态语言列表（Hills 的语言/元数据设置可能用到）。
7. 评估 `/Items/{id}/ThumbnailSet`（Hills 播放页请求，当前 404）。
8. 为 `MediaSources` 补 `ETag`（可选兼容项；这两个客户端不需要，其他 Emby 客户端可能使用）。
9. `/embywebsocket`：至少明确返回 404；若要支持 Yamby 的实时同步，再评估最小握手/事件。
10. 增加回归测试，固定以下客户端查询形态：
    - `Similar?UserId=&Fields=&Limit=`
    - `HideFromResume?Hide=true`（POST，带 `/Users/{id}`）
    - `Genres?...&Limit=`（空值）
    - `Sessions/Playing` 的 `EventName` 只出现在 `/Progress`

## 5. 原始数据

- 抓包日志：`.tmp/capture/proxy.log`（134 条 HTTP 交换，凭据已脱敏；未提交）。
- 截图/OCR：`.tmp/capture/*.png`、`.tmp/capture/*.xml`（未提交）。
- 本地测试实例与数据库在任务结束时已删除；测试账号、token、cookie 文件已删除。
- 报告内所有主机、用户、条目、令牌均以 `<host>`、`<userId>`、`<itemId>`、`<token>` 表示。
