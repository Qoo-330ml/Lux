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

- React Query v5 的 `refetchIntervalInBackground` **默认就是 `false`**，实现上只有 `focusManager.isFocused()` 为真时才会按间隔重取（见 `@tanstack/query-core` 的 `queryObserver`：`if (this.options.refetchIntervalInBackground || focusManager.isFocused())`）。
  本仓库安装的是 `@tanstack/query-core@5.101.4`，其 `FocusManager` 默认**只监听 `visibilitychange`**，`isFocused()` 在未显式设置时回退为 `document.visibilityState !== "hidden"`；仓库内没有调用 `focusManager.setEventListener` 自定义过监听（`grep -rn "focusManager" web/src` 为空）。
  因此实际语义是：**标签页隐藏（`document.hidden === true`）时暂停；窗口可见但失去焦点（例如切到另一个应用窗口）时仍会继续轮询**。任务要求的「页面不可见」正是 `document.hidden`/`visibilityState === "hidden"` 这一条件，所以「查询类轮询无需再加开关」的结论仍然成立。下表「页面不可见时仍轮询」一列即指标签页隐藏时是否仍轮询；本文件把显式写了 `refetchIntervalInBackground: false` 的行标注为「显式」，未写的标注为「默认」。
- 真正会在后台继续跑的是 `window.setInterval`：心跳（必须继续）和客户端统计采样（纯展示，本次改为隐藏时暂停）。
- 用户事件实时通道：`GET /api/v1/ws` 的 `user` 频道（`useUserEvents`），通过 `RealtimeConnectionContext` + `useRealtimeConnectionState()` 暴露连接状态，事件里的 `scope` 会 invalidate `home`、`libraries`、`item`、`item-images`、`playback`、`children`、`favorites`、`collection`、`person`、`library:<id>`。
- 管理事件实时通道：同一条 `GET /api/v1/ws` 连接的 `admin` 频道（`useAdminEvents`），`scope` 取值 `all | dashboard | jobs | libraries | plugins | users | metadata | settings`，按 `queryKeysByScope` invalidate 对应查询。LUX-898 起管理端可以拿到连接状态：`useAdminEvents()` 返回状态，另导出 `useAdminRealtimeConnectionState()` 给管理页与任务活动弹层；它是 **admin 频道**的状态（由管理端自己订阅），不读用户频道的 `RealtimeConnectionState`，非管理员不订阅管理频道。仪表盘、任务活动弹层与运行记录四类任务列表已按“已连接”关闭兜底轮询，其余没有 scope 覆盖的查询继续轮询（见文末「LUX-898」）。

## 逐处清单

| # | 文件:行 | 用途 | 间隔 | 页面不可见时仍轮询 | 已有实时连接可覆盖 | 处理建议 |
|---|---|---|---|---|---|---|
| 1 | `src/features/home/HomePage.tsx:39` | 首页轮播（`homeCarousel`） | 30s；`realtimeState==="connected"` 或无数据时 `false` | 否（显式 `:40`） | 是（`invalidate` 任意 scope 都会刷新 `home`） | 保持：已按实时连接状态关闭兜底轮询 |
| 2 | `src/features/home/HomePage.tsx:40` | 上一条的 `refetchIntervalInBackground: false` | — | 否（显式） | — | 保持 |
| 3 | `src/features/home/HomePage.tsx:47` | 首页媒体库列表（`libraries`） | 同上 | 否（显式 `:48`） | 是 | 保持 |
| 4 | `src/features/home/HomePage.tsx:48` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 5 | `src/features/home/HomePage.tsx:55` | 继续观看（`homeContinueWatching`） | 同上 | 否（显式 `:56`） | 是 | 保持 |
| 6 | `src/features/home/HomePage.tsx:56` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 7 | `src/features/home/HomePage.tsx:66` | 各媒体库最新条目（`homeLatestLibraries`） | 同上 | 否（显式 `:67`） | 是 | 保持 |
| 8 | `src/features/home/HomePage.tsx:67` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 9 | `src/features/auth/SetupPage.tsx:16` | 数据库配置状态（重启后等待迁移完成） | 1s，仅 `restartRequested` 时 | 否（默认） | 无（服务正在重启，实时连接不可用） | 保持：这是重启流程的必要轮询，不能停 |
| 10 | `src/features/activity/ScanActivityPopover.tsx:37` | 管理端任务活动弹层（`adminTaskActivity`） | 5s，仅弹层打开时；**（LUX-898）admin 频道已连接时 `false`** | 否（显式 `:38`） | 是（`jobs` scope） | 保持：已按管理频道连接状态关闭兜底轮询 |
| 11 | `src/features/activity/ScanActivityPopover.tsx:38` | 上一条的后台开关 | — | 否（显式） | — | 保持 |
| 12 | `src/features/admin/EmbyMigrationPluginConfig.tsx:198` | Emby 迁移作业列表（`adminEmbyMigrations`） | 5s，仅历史面板打开时 | 否（默认） | 否：`queryKeysByScope` 未包含 `admin/emby-migrations`，只有连接时 `all` 快照 | 需后端新增 `emby` 事件或把该 key 归入 `jobs` scope 后再关轮询 |
| 13 | `src/features/admin/EmbyMigrationPluginConfig.tsx:205` | 单个迁移作业详情（`adminEmbyMigration`） | 5s，仅作业进行中 | 否（默认） | 否（同上） | 同上 |
| 14 | `src/features/admin/AdminTranscodingPage.tsx:39` | 转码会话列表（`adminTranscodingSessions`） | 2s（全站最密） | 否（默认） | 否：`admin/transcoding/sessions` 不在任何 scope 内 | 建议后续加管理事件 `transcoding` scope（需后端），把 2s 兜底降到 10s 或改为事件驱动 |
| 15 | `src/features/admin/AdminDashboardPage.tsx:16` | 管理仪表盘（`adminDashboard`） | 15s（`liveDashboard`）；**（LUX-898）admin 频道已连接时 `false`** | 否（默认） | 是（`dashboard`/`jobs` scope） | 保持：已按管理频道连接状态关闭兜底轮询 |
| 16 | `src/features/admin/AdminOperationsPage.tsx:190` | STRM 探测任务列表（`adminStrmProbeJobs`） | 5s；**（LUX-898）admin 频道已连接时 `false`** | 否（默认） | 是（`jobs` scope） | 同上（已按管理频道连接状态关闭）；这些查询同时驱动「运行记录」标签页计数，间隔由同一个状态决定，不按标签页关闭 |
| 17 | `src/features/admin/AdminOperationsPage.tsx:196` | 片头片尾检测任务（`adminChapterDetectionJobs`） | 5s；**（LUX-898）admin 频道已连接时 `false`** | 否（默认） | 是（`jobs` scope） | 同上 |
| 18 | `src/features/admin/AdminOperationsPage.tsx:202` | 弹幕匹配任务（`adminDanmakuMatchJobs`） | 5s；**（LUX-898）admin 频道已连接时 `false`** | 否（默认） | 是（`jobs` scope） | 同上 |
| 19 | `src/features/admin/AdminOperationsPage.tsx:208` | 媒体库封面任务（`adminLibraryCoverJobs`） | 5s；**（LUX-898）admin 频道已连接时 `false`** | 否（默认） | 是（`jobs` scope） | 同上 |
| 20 | `src/features/admin/AdminOperationsPage.tsx:214`（现为 `src/features/admin/AdminLibraryDeletionPanel.tsx:20`） | 媒体库删除任务（`adminLibraryDeletionJobs` → `["admin","library-deletion-jobs",…]`） | 有任务 5s / 空闲 15s；**（LUX-898）已连接时不变** | 否（默认） | 否：`libraries` scope 只含 `["admin","libraries"]`，`jobs` scope 也不含 `library-deletion-jobs`，只有连接时 `all` 快照 | 保持轮询；要彻底去掉需后端补 scope 事件（LUX-898 不改后端 scope） |
| 21 | `src/features/admin/user-management/detail/ContentIndexStatus.tsx:9`（拆分前为 `AdminUserManagementPage.tsx:481`） | 用户「内容分类索引」就绪状态（`AdminUserContentIndexStatus` → `api.adminContentRestrictionIndexStatus()`，key `["admin","content-restriction-index"]`） | 5s，仅 `pendingItems` 时 | 否（默认） | 否：`users` scope 只有 `["admin","users"]`、`["admin","user-access"]`、`["admin","libraries"]`、`["admin","dashboard"]`、`["admin","logs"]`，匹配不到 `content-restriction-index` | 保持轮询；要去掉需后端新增（例如 `metadata`/专用）scope 事件。该文件属另一并行任务，本次只记录不改 |
| 22 | `src/features/detail/MediaDetailPage.tsx:19` | 详情页 `item` 查询的间隔常量 | 30s 或 `false` | — | — | 保持 |
| 23 | `src/features/detail/MediaDetailPage.tsx:25` | 详情主数据（`item`） | 30s；连接时 `false` | 否（显式 `:26`） | 是（`item` scope） | 保持：已按实时连接状态关闭兜底轮询 |
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

## 实时连接覆盖核对

「已有实时连接可覆盖」一列按下面的键列表逐条核对（React Query 的 `invalidateQueries({ queryKey })` 是前缀匹配）：

- 用户事件 `useUserEvents.ts`（`GET /api/v1/ws` 的 `user` 频道）：`["home"]`、`["libraries"]`、`["item"]`/`["item", id]`、`["item-images"]`/`["item-images", id]`、`["playback"]`/`["playback", id]`、`["children"]`、`["favorites"]`、`["collection"]`、`["person"]`、`["library"]`/`["library", id]`。
  - 逐条对应：首页四项（`["home","carousel"]`、`["libraries"]`、`["home","continue-watching"]`、`["home","latest-libraries",…]`）落在 `["home"]`/`["libraries"]` 前缀下；详情页 `item`、`children`、`playback`、`item-images` 都直接命中。
- 管理事件 `useAdminEvents.ts`（同一条 `GET /api/v1/ws` 的 `admin` 频道，`queryKeysByScope`）：
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

- 全部 `refetchInterval` 轮询在标签页隐藏时已经暂停（React Query 默认，见上文说明；窗口可见但失焦时仍轮询，这不属于「页面不可见」），因此「页面不可见时暂停纯展示轮询」对查询类轮询**无需改动**；本次只处理了唯一的裸 `setInterval`。
- 心跳是会话保活协议的一部分，必须后台继续，未改动。
- 明细/首页系列轮询都已经按 `useRealtimeConnectionState() === "connected"` 关闭了兜底轮询，没有遗漏点。

## 需要后续任务处理（本次不改后端）

1. **管理端连接状态（已由 LUX-898 完成，2026-11-07）**：`useAdminEvents()` 现在返回连接状态，并导出 `useAdminRealtimeConnectionState()`（admin 频道，独立于用户频道）；管理仪表盘 15s、任务活动弹层 5s、运行记录四类任务列表 5s 在已连接时改为 `refetchInterval: false`，断线、握手失败与 `1008` 之后恢复原间隔。剩余轮询点与原因见文末「LUX-898」。
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

## 后续补充：实时通道迁移（LUX-895 / LUX-897，2026-11-06）

上文的传输描述已按迁移后的现状改写为 WebSocket；SSE 端点（两条独立的事件流）已由 LUX-897 在同一发布版本
中删除，现在只有 `GET /api/v1/ws` 一条实时通道，旧端点返回 `404`。下表列的是迁移前后语义不变的部分：

- 连接状态的名字已改：`SseConnectionState` → `RealtimeConnectionState`、
  `useSseConnectionState()` → `useRealtimeConnectionState()`、`SseConnectionContext` →
  `RealtimeConnectionContext`。上文表格里写 `realtimeState==="connected"` 的行指的是同一个语义——**收到首帧
  `ready` 之后**才算已连接，因此"已连接即关闭兜底轮询"的判断没有变化，只是文件名/行号会整体位移。
- 传输层是 `GET /api/v1/ws`（ADR-054；SSE 两条流已删除），用户频道与管理频道共用一条连接（`web/src/lib/realtime/`）。
  用户侧「已按连接状态关闭兜底轮询」的行为不变；`useAdminEvents` 仍然不导出连接状态，所以管理端的
  2 s / 5 s / 15 s 兜底轮询照旧，上文的「需要后续任务处理」第 1 条仍然成立（由 LUX-898 处理；该条已
  在 2026-11-07 完成，见文末「LUX-898」）。
- WebSocket 不可用（例如环境里没有 `WebSocket` 构造器）时连接保持断开，用户面的 30 s 兜底轮询因此照旧
  生效；断线重连期间同样落到这条兜底。

## 后续补充：管理端连接状态与关闭兜底轮询（LUX-898，2026-11-07）

管理端现在和用户侧一样按“已连接”关闭兜底轮询，但判断来源不同，这一点是本节的关键：

- **状态来自 admin 频道，不来自用户频道**。`useAdminEvents()` 返回 `RealtimeConnectionState`（`AdminLayout`
  使用），另导出 `useAdminRealtimeConnectionState()` 给各个管理面直接调用：任务活动弹层挂在 `LuxShell`
  顶栏，不在 `AdminLayout` 内，只能自己拿状态；仪表盘与运行记录页也用它，不依赖“外层布局已经挂载”的
  隐式前提。两者都调用 `useAdminChannel`，只在管理端挂载时才存在，因此**非管理员根本不订阅管理频道**。
  管理页不读 `useRealtimeConnectionState()`（用户频道上下文）：只看用户频道会得到“已连接但收不到 admin
  帧”。
- **不新开连接**。admin 频道与用户频道共用 `GET /api/v1/ws`（ADR-054 D1），`LuxShell` 通过用户频道的
  `subscribeRealtime` 持有 socket；管理端用新增的 `observeRealtime()`（`web/src/lib/realtime/client.ts`）
  被动接收同一个连接的 `ready` / `invalidate` / 状态事件，不启动、也不保活连接。只有 `useAdminEvents()`
  （即 `AdminLayout`）额外用 `subscribeRealtime` 保证“没有用户频道时控制台仍有一条连接”。
- **只有收到首帧 `ready` 才报 connected**；断线、握手失败（从未 ready）或 `1008` 会话结束都会回到
  `disconnected`，轮询立即恢复。250 ms 合并窗与“每次 open 全量失效”（ADR-054 D15②③）收敛到管理端唯一的
  一次注册里，多个管理组件同时挂载时不会重复失效。

### 现在的管理端轮询

| 查询 | 断线 / 未连接 | admin 频道已连接 | 结论 |
|---|---|---|---|
| `adminDashboard`（仪表盘） | 15 s（`queryRefreshIntervals.liveDashboard`） | `false` | 关闭（`dashboard`/`jobs` scope） |
| `adminTaskActivity`（任务活动弹层，仅在打开时） | 5 s | `false` | 关闭（`jobs` scope） |
| `adminStrmProbeJobs` / `adminChapterDetectionJobs` / `adminDanmakuMatchJobs` / `adminLibraryCoverJobs` | 5 s | `false` | 关闭（`jobs` scope） |
| `adminTranscodingSessions`（转码会话） | 2 s | 2 s | **不变**：`admin/transcoding/sessions` 不在任何 scope 内 |
| `adminEmbyMigrations*`（Emby 迁移作业与详情） | 5 s | 5 s | **不变**：`queryKeysByScope` 没有 `admin/emby-migrations*`，只有连接时的 `all` 快照 |
| `adminLibraryDeletionJobs`（媒体库删除任务，`AdminLibraryDeletionPanel`） | 有任务 5 s / 空闲 15 s | 同左 | **不变**：`jobs` 与 `libraries` scope 都不含 `library-deletion-jobs` |
| `admin/content-restriction-index`（内容分类索引状态） | 5 s（仅 `pendingItems` 时） | 5 s | **不变**：`users`/`metadata` scope 都不含该 key |

后四类保持轮询的原因相同：它们的数据没有被任何管理事件 scope 覆盖，`invalidate` 帧不会因为转码进度、
迁移作业、库删除或索引回填而变化。`all` 帧只在连接建立时和广播 lag 降级时出现，不能当作常态刷新；没有
scope 就等于没有刷新信号，关掉轮询会让这些面板在页面停留期间冻结。补齐它们需要后端新增或扩展事件 scope
（`transcoding`、`emby` 等），超出 LUX-898 的范围（本任务不改后端事件 scope），仍按上文的「管理事件 scope
缺口」处理。

### 已知限制

- **降权后的窗口（ADR-054 D7）**：服务端重校验（周期 45 s）发现管理员降权时只停投 admin 帧、不关闭
  连接，也不下发“频道已停”的帧。客户端没有可判定的信号，会继续报 `connected` 并按“已连接”不轮询，直到
  连接因其它原因断开并重连（或用户打开仍轮询的那几个页面）才纠正。这是信封冻结（D3）的直接代价，不是
  本任务新增的。
- **没有用户频道时**管理页仍保持轮询：admin 观察者不拥有连接，只有 `AdminLayout`（控制台）会主动建连；
  独立渲染的管理页（例如测试）不会自己拉起 socket。

### 复核方式

```bash
cd web
grep -rn "useAdminRealtimeConnectionState\|adminFallbackInterval" src
npx vitest run tests/admin-realtime-polling.test.tsx tests/realtime-client.test.ts tests/admin-events.test.tsx
```

`web/tests/admin-realtime-polling.test.tsx`（9 例）覆盖：仪表盘、任务活动弹层与四类任务列表在“未连接 /
已连接 / 断线恢复 / `1008`”四种情形下的 `refetchInterval`；用户频道上下文声称 connected 时管理页仍轮询；
转码 2 s、媒体库删除任务（有任务 5 s / 空闲 15 s）与内容索引（`pendingItems` 时 5 s）在已连接时不变。
`web/tests/realtime-client.test.ts` 新增 `observeRealtime` 用例：观察者不建连、能收到状态与帧，退出时不
关闭 owner 的 socket。
