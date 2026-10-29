# 前端轮询盘点（C1）

本文件盘点 `web/src` 中所有定时轮询点，并记录本次只做的零风险改动与需要后续任务处理的部分。

## 统计口径

在 `web/` 目录执行：

```bash
grep -rn "refetchInterval\|setInterval" src
```

共 **41 行**命中，即任务描述中的「41 处」：

- 39 行是 `refetchInterval`，其中 24 行是真正的「查询轮询配置」，其余 15 行是同一配置的变量引用 / `refetchIntervalInBackground: false` 配套行；
- 2 行是 `window.setInterval`（播放器统计采样、播放会话心跳）。

去重后实际轮询点 **24 个查询 + 2 个定时器 = 26 个**，下表逐行列出全部 41 行，便于按行号复核。

## 关键前提

- React Query v5 的 `refetchIntervalInBackground` **默认就是 `false`**，实现上只有 `focusManager.isFocused()` 为真时才会按间隔重取（见 `@tanstack/query-core` 的 `queryObserver`：`if (this.options.refetchIntervalInBackground || focusManager.isFocused())`）。因此**所有 `refetchInterval` 轮询在标签页不可见/窗口失焦时已经暂停**，不需要再加开关；本文件把显式写了 `refetchIntervalInBackground: false` 的行标注为「显式」，未写的标注为「默认」。
- 真正会在后台继续跑的是 `window.setInterval`：心跳（必须继续）和客户端统计采样（纯展示，本次改为隐藏时暂停）。
- 用户事件 SSE：`/api/v1/events`（`useUserEvents`），通过 `SseConnectionContext` + `useSseConnectionState()` 暴露连接状态，事件里的 `scope` 会 invalidate `home`、`libraries`、`item`、`item-images`、`playback`、`children`、`favorites`、`collection`、`person`、`library:<id>`。
- 管理事件 SSE：`/api/v1/admin/events`（`useAdminEvents`），`scope` 取值 `all | dashboard | jobs | libraries | plugins | users | metadata | settings`，按 `queryKeysByScope` invalidate 对应查询；**当前没有暴露连接状态**，所以前端无法按连接状态关闭兜底轮询。

## 逐处清单

| # | 文件:行 | 用途 | 间隔 | 页面不可见时仍轮询 | 已有 SSE 可覆盖 | 处理建议 |
|---|---|---|---|---|---|---|
| 1 | `src/features/home/HomePage.tsx:39` | 首页轮播（`homeCarousel`） | 30s；`sseState==="connected"` 或无数据时 `false` | 否（显式 `:40`） | 是（`invalidate` 任意 scope 都会刷新 `home`） | 保持：已按 SSE 连接状态关闭兜底轮询 |
| 2 | `src/features/home/HomePage.tsx:40` | 上一条的 `refetchIntervalInBackground: false` | — | 否（显式） | — | 保持 |
| 3 | `src/features/home/HomePage.tsx:47` | 首页媒体库列表（`libraries`） | 同上 | 否（显式 `:48`） | 是 | 保持 |
| 4 | `src/features/home/HomePage.tsx:48` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 5 | `src/features/home/HomePage.tsx:55` | 继续观看（`homeContinueWatching`） | 同上 | 否（显式 `:56`） | 是 | 保持 |
| 6 | `src/features/home/HomePage.tsx:56` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 7 | `src/features/home/HomePage.tsx:66` | 各媒体库最新条目（`homeLatestLibraries`） | 同上 | 否（显式 `:67`） | 是 | 保持 |
| 8 | `src/features/home/HomePage.tsx:67` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 9 | `src/features/auth/SetupPage.tsx:16` | 数据库配置状态（重启后等待迁移完成） | 1s，仅 `restartRequested` 时 | 否（默认） | 无（服务正在重启，SSE 不可用） | 保持：这是重启流程的必要轮询，不能停 |
| 10 | `src/features/activity/ScanActivityPopover.tsx:37` | 管理端任务活动弹层（`adminTaskActivity`） | 5s，仅弹层打开时 | 否（显式 `:38`） | 是（`jobs` scope） | 保持；如需进一步降载，可在管理 SSE 连接状态可用后关闭兜底轮询（见「后续任务」） |
| 11 | `src/features/activity/ScanActivityPopover.tsx:38` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 12 | `src/features/admin/EmbyMigrationPluginConfig.tsx:198` | Emby 迁移作业列表（`adminEmbyMigrations`） | 5s，仅历史面板打开时 | 否（默认） | 否：`queryKeysByScope` 未包含 `admin/emby-migrations`，只有连接时 `all` 快照 | 需后端新增 `emby` 事件或把该 key 归入 `jobs` scope 后再关轮询 |
| 13 | `src/features/admin/EmbyMigrationPluginConfig.tsx:205` | 单个迁移作业详情（`adminEmbyMigration`） | 5s，仅作业进行中 | 否（默认） | 否（同上） | 同上 |
| 14 | `src/features/admin/AdminTranscodingPage.tsx:39` | 转码会话列表（`adminTranscodingSessions`） | 2s（全站最密） | 否（默认） | 否：`admin/transcoding/sessions` 不在任何 scope 内 | 建议后续加管理事件 `transcoding` scope（需后端），把 2s 兜底降到 10s 或改为事件驱动 |
| 15 | `src/features/admin/AdminDashboardPage.tsx:16` | 管理仪表盘（`adminDashboard`） | 15s（`liveDashboard`） | 否（默认） | 是（`dashboard`/`jobs` scope） | 可在管理 SSE 连接状态可用后关闭兜底轮询；连接状态当前未暴露 |
| 16 | `src/features/admin/AdminOperationsPage.tsx:190` | STRM 探测任务列表（`adminStrmProbeJobs`） | 5s | 否（默认） | 是（`jobs` scope） | 同上；注意这些查询同时驱动「运行记录」标签页计数，不能在非当前标签页直接关掉 |
| 17 | `src/features/admin/AdminOperationsPage.tsx:196` | 片头片尾检测任务（`adminChapterDetectionJobs`） | 5s | 否（默认） | 是（`jobs` scope） | 同上 |
| 18 | `src/features/admin/AdminOperationsPage.tsx:202` | 弹幕匹配任务（`adminDanmakuMatchJobs`） | 5s | 否（默认） | 是（`jobs` scope） | 同上 |
| 19 | `src/features/admin/AdminOperationsPage.tsx:208` | 媒体库封面任务（`adminLibraryCoverJobs`） | 5s | 否（默认） | 是（`jobs` scope） | 同上 |
| 20 | `src/features/admin/AdminOperationsPage.tsx:214` | 媒体库删除任务（`adminLibraryDeletionJobs` → `["admin","library-deletion-jobs",…]`） | 5s，仅「运行记录」标签页 | 否（默认） | 否：`libraries` scope 只含 `["admin","libraries"]`，`jobs` scope 也不含 `library-deletion-jobs`，只有连接时 `all` 快照 | 保持（已按标签页关闭）；若要彻底去掉需后端补 scope 事件 |
| 21 | `src/features/admin/user-management/detail/ContentIndexStatus.tsx:9`（拆分前为 `AdminUserManagementPage.tsx:481`） | 用户「内容分类索引」就绪状态（`AdminUserContentIndexStatus` → `api.adminContentRestrictionIndexStatus()`，key `["admin","content-restriction-index"]`） | 5s，仅 `pendingItems` 时 | 否（默认） | 否：`users` scope 只有 `["admin","users"]`、`["admin","user-access"]`、`["admin","libraries"]`、`["admin","dashboard"]`、`["admin","logs"]`，匹配不到 `content-restriction-index` | 保持轮询；要去掉需后端新增（例如 `metadata`/专用）scope 事件。该文件属另一并行任务，本次只记录不改 |
| 22 | `src/features/detail/MediaDetailPage.tsx:19` | 详情页 `item` 查询的间隔常量 | 30s 或 `false` | — | — | 保持 |
| 23 | `src/features/detail/MediaDetailPage.tsx:25` | 详情主数据（`item`） | 30s；连接时 `false` | 否（显式 `:26`） | 是（`item` scope） | 保持：已按 SSE 连接状态关闭兜底轮询 |
| 24 | `src/features/detail/MediaDetailPage.tsx:26` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 25 | `src/features/detail/useMediaEpisodes.ts:12` | 剧集查询的间隔常量 | 30s 或 `false` | — | — | 保持 |
| 26 | `src/features/detail/useMediaEpisodes.ts:17` | 季列表（`children` SEASON） | 30s；连接时 `false` | 否（显式 `:18`） | 是（`children` scope） | 保持 |
| 27 | `src/features/detail/useMediaEpisodes.ts:18` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 28 | `src/features/detail/useMediaEpisodes.ts:46` | 剧集分页列表（`children` EPISODE） | 30s；连接时 `false` | 否（显式 `:47`） | 是（`children` scope） | 保持 |
| 29 | `src/features/detail/useMediaEpisodes.ts:47` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 30 | `src/features/detail/MediaDetailActions.tsx:56` | 播放进度查询的间隔常量 | 30s 或 `false` | — | — | 保持 |
| 31 | `src/features/detail/MediaDetailActions.tsx:60` | 详情页播放进度（`playback`） | 30s；连接时 `false` | 否（显式 `:61`） | 是（`playback` scope） | 保持 |
| 32 | `src/features/detail/MediaDetailActions.tsx:61` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 33 | `src/features/detail/MediaDetailSections.tsx:24` | 剧照/图片（`itemImages`） | 30s；连接时 `false` | 否（显式 `:25`） | 是（`item-images` scope） | 保持 |
| 34 | `src/features/detail/MediaDetailSections.tsx:25` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 35 | `src/features/detail/MediaDetailHero.tsx:35` | 所属剧集查询的间隔常量 | 30s 或 `false` | — | — | 保持 |
| 36 | `src/features/detail/MediaDetailHero.tsx:39` | 所属剧集（`item` series） | 30s；连接时 `false` | 否（显式） | 是（`item` scope） | 保持 |
| 37 | `src/features/detail/MediaDetailHero.tsx:89` | 标志图查询的间隔常量 | 30s 或 `false` | — | — | 保持 |
| 38 | `src/features/detail/MediaDetailHero.tsx:92` | 标志图（`itemImages` + `select`） | 30s；连接时 `false` | 否（显式） | 是（`item-images` scope） | 保持 |
| 39 | `src/features/player/usePlayerInfoPanel.ts:67` | 播放会话服务端统计（`webPlaybackStats`） | 1s，仅信息面板打开时 | 否（默认） | 否（会话私有数据，无事件） | 保持；面板关闭即停 |
| 40 | `src/features/player/usePlayerInfoPanel.ts:84` | 客户端统计采样（纯展示） | 1s，仅信息面板打开时 | **是（`window.setInterval`）→ 本次改为隐藏时跳过** | 不适用 | 已在 `document.hidden` 时跳过采样，见下 |
| 41 | `src/features/player/usePlaybackReporting.ts:137` | 播放会话心跳（`webPlaybackHeartbeat`） | 60s，有会话时 | **是（有意为之）** | 不适用（这是服务端会话保活协议） | 保持：后台必须继续，不能暂停 |

## SSE 覆盖核对

「已有 SSE 可覆盖」一列按下面的键列表逐条核对（React Query 的 `invalidateQueries({ queryKey })` 是前缀匹配）：

- 用户事件 `useUserEvents.ts`（`/api/v1/events`）：`["home"]`、`["libraries"]`、`["item"]`/`["item", id]`、`["item-images"]`/`["item-images", id]`、`["playback"]`/`["playback", id]`、`["children"]`、`["favorites"]`、`["collection"]`、`["person"]`、`["library"]`/`["library", id]`。
  - 逐条对应：首页四项（`["home","carousel"]`、`["libraries"]`、`["home","continue-watching"]`、`["home","latest-libraries",…]`）落在 `["home"]`/`["libraries"]` 前缀下；详情页 `item`、`children`、`playback`、`item-images` 都直接命中。
- 管理事件 `useAdminEvents.ts`（`/api/v1/admin/events`，`queryKeysByScope`）：
  - `dashboard`：`["admin","dashboard"]`、`["admin","logs"]`
  - `jobs`：`["admin","jobs"]`、`["admin","metadata-jobs"]`、`["admin","strm-probe-jobs"]`、`["admin","chapter-detection-jobs"]`、`["admin","danmaku-match-jobs"]`、`["admin","library-cover-jobs"]`、`["admin","task-activity"]`、`["admin","scheduled-tasks"]`、`["admin","dashboard"]`、`["admin","logs"]`
  - `libraries`：`["admin","libraries"]`、`["admin","scheduled-tasks"]`、`["admin","dashboard"]`、`["admin","logs"]`
  - `plugins`：`["admin","plugins"]`、`["admin","libraries"]`、`["admin","logs"]`
  - `users`：`["admin","users"]`、`["admin","user-access"]`、`["admin","libraries"]`、`["admin","dashboard"]`、`["admin","logs"]`
  - `metadata`：`["admin","logs"]`
  - `settings`：`["admin","settings"]`、`["admin","dashboard"]`、`["admin","logs"]`
  - 逐条对应：任务活动、仪表盘、四类任务列表（`strm-probe-jobs`/`chapter-detection-jobs`/`danmaku-match-jobs`/`library-cover-jobs`）命中 `jobs`/`dashboard`；`admin/emby-migrations`、`admin/transcoding/sessions`、`admin/library-deletion-jobs`、`admin/content-restriction-index` **都不在任何 scope 内**（只有连接时 `all` 快照，以及服务端 lag 时广播的 `all` 能覆盖），所以表中这四类标为「否」。核对时已修正媒体库删除任务（原写「部分」）与内容分类索引状态（原误标「是」）两行。

## 本次零风险改动

### 1. 播放器客户端统计采样在页面不可见时暂停

- 位置：`web/src/features/player/usePlayerInfoPanel.ts`
- 改动：采样 `setInterval` 的回调在 `document.hidden` 时直接返回，不调用采样器；面板关闭、会话切换等原有停止条件不变。
- 为什么不改变用户可见行为：该采样只驱动「信息面板」里的客户端统计文本（帧率/缓冲/丢帧），页面不可见时没有任何像素会被用户看到；页面重新可见后最多 1 秒就会拿到新样本，与改动前的显示一致（改动前隐藏期间产生的样本也从未被看到）。
- 测试：`web/tests/player-info-hook.test.tsx`（`keeps sampling paused while the tab is hidden`）。

### 2. 统计查询的间隔配置去掉冗余三元表达式

- 位置：`web/src/features/player/usePlayerInfoPanel.ts`
- 改动：`refetchInterval: showInfo ? 1_000 : false` → `refetchInterval: 1_000`。
- 为什么不改变行为：同一查询的 `enabled` 已经是 `showInfo && Boolean(playbackSessionId)`，`showInfo` 为假时查询本身被禁用，`refetchInterval` 不会生效。
- 测试：`web/tests/player-info-hook.test.tsx`（面板关闭后不再请求）。

### 未改动但已确认的部分

- 全部 `refetchInterval` 轮询在后台已经暂停（React Query 默认），因此「页面不可见时暂停纯展示轮询」对查询类轮询**无需改动**；本次只处理了唯一的裸 `setInterval`。
- 心跳是会话保活协议的一部分，必须后台继续，未改动。
- 明细/首页系列轮询都已经按 `useSseConnectionState() === "connected"` 关闭了兜底轮询，没有遗漏点。

## 需要后续任务处理（本次不改后端）

1. **管理端 SSE 连接状态**：`useAdminEvents` 不返回连接状态，导致管理页无法像用户侧那样在 SSE 已连接时关闭兜底轮询。建议在 SSE 语义任务中导出 `useAdminSseConnectionState()`（新增状态，不改事件语义），随后可关闭：仪表盘 15s、任务活动 5s、运行记录四类任务列表 5s。
2. **管理事件 scope 缺口**：
   - 转码会话（`admin/transcoding/sessions`，2s）没有任何 scope，需要后端新增 `transcoding` 事件；
   - Emby 迁移作业（`admin/emby-migrations*`）不在 `queryKeysByScope` 中，需要新增 `emby` scope 或归入 `jobs`；
   - 媒体库删除任务（`admin/library-deletion-jobs`）与内容分类索引状态（`admin/content-restriction-index`）同样不在任何 scope 内，需要后端补 scope 事件。
   这些都需要后端事件，属于「新增后端事件」范畴，本次只记录。
3. **间隔过密**：`AdminTranscodingPage` 的 2s 是当前最密的前端轮询，在 scope 可用前不建议直接降间隔（会影响转码状态的实时性）。
4. **用户管理页**（拆分前 `AdminUserManagementPage.tsx:481`，现 `web/src/features/admin/user-management/detail/ContentIndexStatus.tsx:9`）归另一并行任务，本次未触碰。

## 复核方式

```bash
cd web
grep -rn "refetchInterval\|setInterval" src   # 41 行，与本文件清单一一对应
npx vitest run tests/player-info-hook.test.tsx
```
