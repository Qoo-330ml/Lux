# 评审存档：Lux Web 实时通道 SSE → WebSocket 迁移盘点（2026-11-06）

评审对象：只读设计盘点任务（讨论子会话，DeepSeek Harness），基线为本地 `test`（含 LUX-887 至 LUX-890，LUX-891 尚未合并）。
评审方式：子会话只读代码并给出方案；父会话逐条判断，**未改动任何代码**。

## 父会话逐条处理

| 子会话意见 | 处理 | 落点 |
|---|---|---|
| 仓库没有 `ALLOWED_ORIGIN`/CORS，Origin 策略需要首次定义 | 采纳。作为 T3 的安全前提，并写入 ADR | T0 / T3 |
| 现有 SSE 无断线续传，WebSocket 不补 | 采纳。重连时前端本来就全量失效 | 协议设计 |
| 管理端 SSE 不暴露连接状态，管理页轮询从未关闭，收益可能大于换协议 | 采纳，拆成独立小任务（不绑定迁移） | 独立任务 |
| 方案 A（单端点 `/api/v1/ws` + `channel` 字段，服务端按主体决定频道） | 作为推荐方案，待所有者确认 | T0 |
| 先合并 LUX-891，再抽取共享基础设施（budget/shutdown/send），不复制 | 采纳 | T1/T2 |
| 双轨一个版本（WS 优先，失败回退 SSE）再删除 SSE | 作为默认建议，待所有者确认 | T0 / T6 |
| nginx 样例与 smoke 配置缺 Upgrade 头，缺失时表现为 close 1006 且与 401 无法区分 | 采纳，必须与后端同一批提交，不得后置 | T5 |
| 连接预算 64 偏小，应拆分用户/管理预算并提高 | 采纳，在 T3 中处理，具体数字在 ADR 中写明依据 | T3 |
| 迁移时不移植 `libraryId` 死代码分支 | 采纳 | T4 |
| 合并通道使一次反代故障同时影响用户面和管理面 | 记为主动取舍，写入 ADR | T0 |
| 保留 SSE、仅删除轮询兜底可用更少代码达到相同运维收益 | 记入 ADR 的“已评估替代方案”；所有者已决定迁移，不采纳为主线 | T0 |

## 子会话最终回复原文

## 问题理解

目标是给“Lux 自己的 Web 实时通道”做一次只读盘点：把 `GET /api/v1/events` 与 `GET /api/v1/admin/events` 从 SSE 迁到 WebSocket，复用（或先抽取）LUX-891 在 `src/api/emby_websocket.rs` 里已经打磨过的连接预算、关停、写边界、周期重校验，并说清协议、安全、代理、前端、文档和任务顺序。

读代码时我核对出三处前提需要修正，后面的结论都基于修正后的事实：

1. **本快照里没有任何 `ALLOWED_ORIGIN` 或等价配置。** `grep -rni "origin" src/api/*.rs` 只命中了 `original`/`OriginalTitle` 之类；`src/config/mod.rs` 只读 `LUX_HTTP_ADDR`、`LUX_CONFIG_DIR`（另加数据库相关变量）。全仓库也没有 CORS 层。所以题目里“与现有 `ALLOWED_ORIGIN`/反向代理配置如何一致”没有现成答案——Origin 策略必须在本任务里**首次定义**，这是安全章节最大的缺口，而不是对齐问题。
2. **现有 SSE 从来没有“断线续传”。** 所有帧都是 `event: invalidate\ndata: {...}\n\n`，没有 `id:` 字段，没有 `retry:` 指令（`src/api/admin/admin_logs_events.rs:449,504,576`）；前端也从不读 `lastEventId`。`Last-Event-ID` 在 WebSocket 里需要自己设计——但**现有实现根本没用到，也不需要补**。
3. **管理端 SSE 目前没有暴露连接状态**，所以管理页的兜底轮询从来不会关闭（`docs/WEB-POLLING-AUDIT.md:27,115`）。如果迁移的真实动机是降负载，这一点比换传输协议的收益更大，建议一并处理。

---

## 方案

### 0. 现状清单（Q1，已核对）

**后端端点**（仅两个，都是 GET + SSE）

| 端点 | handler | 鉴权 | 预算 | 首帧 | 心跳 |
|---|---|---|---|---|---|
| `/api/v1/events` | `user_events` `admin_logs_events.rs:484-514` | `require_web_user`（`src/api/users.rs:2813`，接受 session cookie **或**客户端令牌） | `state.sse_permits`（`:492`） | `event: ready` + `{"version":1}`（`:504`） | 15 s `: keep-alive`（`:533`） |
| `/api/v1/admin/events` | `admin_events` `:414-466` | `require_admin(.., false)`（`src/api/admin/admin_common.rs:3`，不校验 CSRF） | 同一信号量（`:421`） | 同上（`:431`） | 15 s（`:456`） |

- 路由注册在 `src/api/admin/admin_logs_events.rs:5-13`。响应头由 `event_stream_response`（`:468-482`）统一设置：`text/event-stream; charset=utf-8`、`Cache-Control: no-cache, no-store, must-revalidate`、`X-Accel-Buffering: no`。
- 预算：`MAX_SSE_CONNECTIONS = 64`（`src/api/legacy.rs:1369`），`SseConnectionBudget`（`:165-177`），**用户流和管理流共用一个信号量**；超限返回 `429` + `Retry-After: 30`（`sse_capacity_response` `:1811-1823`）。
- 注意：`/api/v1/playback/sessions/{id}/events`（`src/api/lux_api.rs:97`）和 `/api/v1/admin/jobs/{id}/events`（`src/api/admin/admin_jobs.rs:90`）是普通 JSON POST/GET，**不是 SSE**，迁移时不要误伤。

**事件类型与 scope**（`src/application/admin_events.rs`）

- 用户：`UserEventScope`（`:26-34`）= `home | item | library | favorites | progress`；线上只有 `{"scope":...}` 和可选的 `itemId`（`:563-577`）。`libraryId` 在前端解析器里存在（`useUserEvents.ts:130`），但**服务端从不发送**，是死代码。
- 管理：`AdminEventScope`（`:14-22`）= `all | dashboard | jobs | libraries | plugins | users | metadata | settings`，线上只有 `{"scope":...}`（`:448`）。
- 用户级事件靠 `UserEvent.user_id` + `is_visible_to`（`:119`）在订阅端过滤；`subscribe_for`（`:302`）。
- 合并与降级：`EVENT_CHANNEL_CAPACITY = 256`（`:11`）；home 1 s 合并窗（`:12`）；jobs 1 s；丢帧时用户流降级为 `home`（`:546-556`），管理流降级为 `all`（`:445`）。
- 管理事件会**顺带转发**成用户 scope：`publish_user_invalidation`（`:209-222`），映射为 Metadata/Jobs→`item`、Libraries→`library`、Users/Settings→`home`、Dashboard/Plugins→不转发。这条隐式耦合必须在协议设计里保留，否则普通用户会漏刷新。

**触发源**（22 处调用点，9 个文件）

| 文件 | 事件 |
|---|---|
| `src/api/admin/admin_logs_events.rs:185,259` | `Dashboard`（审计/活动写成功后） |
| `src/api/playback.rs:1917,5028,5036,5355,5495,5601,5647` | `home`（合并）、`progress`（带 itemId）、`item`、`favorites` |
| `src/api/users.rs:2174,2292,2547` | `home`（合并，账户偏好） |
| `src/api/emby_resume_visibility.rs:69,71` | `home`（合并，Emby 侧可见性） |
| `src/api/legacy.rs:2219` | `favorites` |
| `src/application/scanner/jobs/{lifecycle.rs:342,1338,1340,497, local_metadata.rs:169,239,350,418,888,1441, manifest_discovery.rs:1405, postprocessing.rs:1407}` | `home`、`Jobs` |
| `src/application/reidentify.rs`（10 处） | `Jobs` |
| `src/application/identity_merge.rs:101` | `Metadata` |

管理 scope 由 `admin_event_scope_for_audit`（`:87-112`）按审计事件前缀派生，新增 scope 时改这里即可。

**前端消费方**

| 位置 | 作用 |
|---|---|
| `web/src/components/layout/LuxShell.tsx:48` → `useUserEvents()` | 每个标签页**一条**用户流；`SseConnectionContext`（`useUserEvents.ts:13`） |
| `AdminLayout.tsx:32` → `useAdminEvents()` | 管理页**再开一条**管理流（`App.tsx:141` 把 `/admin` 包在 `LuxShell` 里，所以管理页同时有 2 条） |
| `HomePage.tsx:31,157` | `homeRefetchInterval(data, sseState)`：连上 → `false`，否则 `30_000`（`query-keys.ts:122 mediaSurface`） |
| `MediaDetailPage.tsx:18`、`useMediaEpisodes.ts:11`、`MediaDetailActions.tsx:55`、`MediaDetailSections.tsx:20`、`MediaDetailHero.tsx:34,96` | 同上，共 7 处 `useSseConnectionState()` 调用 |
| `AdminLayout.tsx` | **没有**连接状态，管理端轮询（2 s 转码、4×5 s 任务列表）永不关闭 |

**相关测试**

- Rust：`tests/admin/admin_events.rs`（5 例，含 `concurrent_event_streams_are_capped`）、`tests/admin/user_event_scoping.rs`（2 例）、`tests/admin/user_query_events.rs`（1 例）、`tests/playback/playback_webhooks.rs`（1 处断言）；单测 `user_event_frame_tests`（`admin_logs_events.rs:579-660`），含“lagged 时写 home 兜底帧”。
- Web：`tests/user-events.test.tsx`（169 行，16 处 `FakeEventSource`）、`tests/admin-events.test.tsx`（163 行，10 处）、`tests/lux-shell.test.tsx`（668 行，18 处）、`tests/library-page.test.ts`（1 处）。四个文件各自手写了一份 Fake，迁移时需要先合并成一个共享 helper。

---

### 1. 协议设计（Q2）——三个可选方案

**共同的信封**（推荐，最小语义漂移）：

```json
{"type":"ready","version":1}
{"type":"invalidate","channel":"user","scope":"progress","itemId":"<id>"}
{"type":"invalidate","channel":"admin","scope":"jobs"}
```

`scope` 的字符串值**逐字不变**，`itemId` 可选；只新增 `type` 与 `channel`。这样 `parseUserEvent` / `parseScope` 的校验逻辑可以原样保留，只在入口加一句 `channel` 判定，前端改动面最小。`ready` 保留 `{"version":1}` 的现有形状（`docs/API.md:157`），版本协商只发生在首帧。

**方案 A：新路径 `/api/v1/ws`，单连接，服务端决定频道**

- 做法：握手时按 session 主体算 `can_manage_server`，据此决定是否投递 `channel:"admin"` 帧。客户端**不发任何订阅消息**，入站只允许 `{"type":"ping"}`（可选）。每 45 s 重校验一次主体（复用 LUX-891 节奏），管理员被降权时只停投 admin 帧，不关连接。
- 代价：新路径 + 新 handler + 一套 Origin 校验；协议多一个 `channel` 字段。
- 风险：单连接故障会同时影响用户面和管理面；授权逻辑集中在握手与重校验两处，测试必须覆盖“降权后不再收到 admin 帧但继续收到 user 帧”。

**方案 B：两个独立端点 `/api/v1/ws` + `/api/v1/admin/ws`，各自单频道**

- 做法：与今天 1:1 对应，鉴权各自用 `require_web_user` / `require_admin`，前端两个 hook 各连一条。
- 代价：管理页每标签页 2 条连接，64 的预算更快见底；两套 handler 或一个带参数的 handler。
- 风险：小；两条流互不牵连，可以做“先迁管理通道”的分阶段上线。

**方案 C：沿用 `/api/v1/events`，同路径按 `Upgrade` 头分流**

- 做法：同一 handler 内判断 `Upgrade: websocket` 走 WS，否则走现有 SSE，天然支持灰度。
- 代价：handler 变分支，SSE/WS 两套帧渲染并存，测试要覆盖两条路径；半年后删除 SSE 时才简化。
- 风险：路径语义含混（“events”到底是流还是升级），`docs/API.md` 要写成双模式，后续读者容易误解。

**心跳与重连 / 续传**

- 心跳：服务端 25 s ping + 120 s 无入站帧关闭（沿用 `EMBY_WEBSOCKET_HEARTBEAT_INTERVAL` / `EMBY_WEBSOCKET_IDLE_TIMEOUT`，`emby_websocket.rs:34,36`）。浏览器 JS **看不见 ping**，所以客户端不能靠它判活；判活靠“服务端 idle 关闭 → 客户端收到 close → 走退避重连”，这已经优于 SSE（SSE 的注释心跳同样不会触发浏览器超时）。
- 续传：**不做**。理由见“问题理解”第 2 条——现状没有任何 resume 语义，所有事件都是幂等的“去重读这个缓存”，两个 hook 在重连时本来就会全量失效（`useUserEvents.ts:96-101`、`useAdminEvents.ts:103`）。补一个 `seq` 只会增加测试面而不改变正确性。可选：服务端在 `ready` 里带一个 `connectionId`，仅用于日志关联，不参与重放。

---

### 2. 安全（Q3）

- **Origin 校验是必做项，且是本任务新增的基础设施。** RFC 6455 §4.1 要求服务端自行校验；浏览器 WebSocket 不受同源策略约束。建议策略：
  - 接受条件 = `Origin` 的 scheme+host+port 与本次请求的 `Host`（scheme 取 `X-Forwarded-Proto`，缺省按明文）完全一致；**或**命中新增的 `LUX_ALLOWED_ORIGINS`（逗号分隔、精确匹配、最多 10 条、不接受通配符）。
  - 缺失 `Origin` 时：cookie 认证的握手**拒绝**；仅当主体来自客户端令牌（`lux_user_token` / `X-Lux-Api-Key`，`users.rs:2862`）时放行，供脚本/测试使用。
  - 附带检查：`Sec-Fetch-Site` 存在且为 `cross-site` 时拒绝（廉价信号，不能当唯一控制）。
  - 为什么必须留 `LUX_ALLOWED_ORIGINS`：部分 NAS 反代会重写 `Host`，此时严格同源会**拒绝所有握手**。样例 nginx 配置有 `proxy_set_header Host $host`（`docs/DEPLOYMENT.md:226`），但无法保证所有部署都如此。
- **握手认证**：浏览器不能给 WS 加自定义头，所以只能走 cookie。`lux_session` 是 `HttpOnly; SameSite=Lax`（`admin_common.rs:289`；`docs/API.md:53`），`require_web_principal` 的 cookie 分支（`users.rs:2942-2966`）可直接复用。**禁止**把 token 放进 query（会被 access log、反代日志、浏览器历史记录）。
- **CSRF 等价防护**：握手是 GET，带不了 `X-CSRF-Token`，所以 `SameSite=Lax` + Origin 校验就是全部 CSRF 故事，必须在 API 文档里写清楚“WS 端点不要求也不接受 CSRF 令牌，改由 Origin 校验”。`SameSite=Lax` 已经能挡住绝大多数跨站 WS（跨站握手不带 Lax cookie），但它不能替代 Origin 校验（浏览器行为差异 + 未来若改 SameSite=None 就全线失守）。
- **管理员通道授权**：只从 `principal.can_manage_server` 推导，绝不接受客户端声明的频道；45 s 周期重校验 + 每次批量投递前复用已缓存的校验结果；降权时停投 admin 帧并可选下发 `{"type":"channel","channel":"admin","state":"revoked"}`，但**不关连接**（否则用户面会跟着断）。
- **入站一律不可信**：`max_message_size`/`max_frame_size` 设 4 KiB（比 Emby 的 8 KiB 更严，因为我们只需要 ping）；二进制帧直接 `1003` 关闭；未知 `type` 忽略；不做任何基于客户端 ID 的查询。
- **日志脱敏**：只记 `user_id`、连接序号、close code、`channel`；不记 cookie、不记完整 URI（只记 path）、不记 query。`Origin` 只在 debug 级别记录，且它会泄露内网主机名。

---

### 3. 资源与关停（Q4）

**LUX-891 的可复用度**（`src/api/emby_websocket.rs`，1003 行）

| 组件 | 行 | 复用判断 |
|---|---|---|
| `EmbyWebsocketConnections` / `Permit`（全局信号量 + 每用户计数 + `wait_until_idle`） | `:153-248` | **纯通用**，只改名即可 |
| `EmbyWebsocketShutdown`（`watch<bool>`） | `:250-272` | **纯通用** |
| `bounded` / `send_bounded` / `payload_fits` / `utf8_close` / close code | `:464-505,764-772` | **纯通用** |
| `run_connection`（定时器前置、idle、heartbeat、重校验、select shutdown/inbound/sleep） | `:506-734` | **形状通用**，但唤醒源是 `StreamState`（轮询计划），Lux 要换成 `broadcast::Receiver` |
| `revalidated_principal` / `handle_client_message` / `StreamTiming` / `emby_*_entries` | `:756-812,814-852` | Emby 专有 |

**抽取点**：新建 `src/api/websocket/{mod.rs,budget.rs,shutdown.rs,send.rs}`，`emby_websocket.rs` 改为 `use super::websocket::*`。约 300 行移动、零行为变更。

**合并冲突与顺序**：LUX-891 改动 `src/api/legacy.rs`（`CoreState` 两个字段 + 4 个 builder，`:758-790`）、`src/api/mod.rs`（`pub use`）、`src/main.rs`（`:337`、`:419` 两处关停调用）、`Cargo.toml`。今天从 `test` 切出的迁移分支必然在这 4 个文件的同一区域冲突。**先合 LUX-891，再做抽取**，抽取就退化为纯 rename/move，且 LUX-891 的 22 个集成测试 + 9 个单测就是回归网。

**多标签页与预算**：单播设计下每标签页 1 条 WS；两端口设计下管理页 2 条。今天 64 的全局预算 = 约 30 个管理标签页就能让所有人 429，然后静默回落到轮询。迁移时应同时做两件事：预算提到 256、按用户上限 ≥8（NAS 用户常开多标签），并**把用户流与管理流拆成两个预算**，否则管理端突发会挤掉普通用户。

**HTTP/1.1 与 HTTP/2**：`luxd` 只提供明文 HTTP（`docs/DEPLOYMENT.md:241` 明确写“`luxd` 自身仍监听明文 HTTP/1.1”），浏览器侧是 HTTP/1.1 经典 `Upgrade` 握手；nginx 开 `http2 on` 时客户端是 h2、上游仍是 `proxy_http_version 1.1`，由 nginx 做 h1 升级翻译。**任何环节都不需要 RFC 8441**。真实差异是：SSE 会占用浏览器每源约 6 条 HTTP/1.1 连接之一，WebSocket 升级后脱离该连接池（通用浏览器行为，本次未实测）。

**反向代理**：`docs/DEPLOYMENT.md:213-238` 的样例和 `scripts/proxy-smoke.nginx.conf` **都缺** `proxy_set_header Upgrade $http_upgrade;` 与 `proxy_set_header Connection "upgrade";`。缺失时 upgrade 头被剥掉，请求以普通 GET 到达，`WebSocketUpgrade` 提取器直接 400，浏览器侧表现为 `close 1006`——**与 401 完全无法区分**。Caddy 侧仓库里没有配置，`reverse_proxy` 默认支持升级（`docs/COMPATIBILITY.md:1467` 明确说明 Caddy/Hills 链路未实机验证）。中间件本身安全：压缩谓词只压 `application/json`（`src/api/routes.rs:107-121`），ETag 中间件对非 2xx 直接返回（`:26`），101 不会被缓冲。

---

### 4. 前端（Q5）

新增 `web/src/lib/realtime/socket.ts`（约 200 行），`useUserEvents` / `useAdminEvents` 只换传输层，**失效逻辑逐字保留**。

- URL：`${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/api/v1/ws`，不带任何凭据。
- 退避：`min(30s, 500ms × 2^n)` + ±20% 抖动；收到首个 `ready` 才把 `n` 归零。
- 可见性/离线：`document.hidden` 时暂停重连、`visibilitychange` 可见时立即重试一次；`offline` 时暂停、`online` 时重试。否则合盖的笔记本会持续累积定时器。
- 鉴权失效：`1008`（策略违规/会话撤销）→ 不再重连，主动 `invalidateQueries(queryKeys.me)` 让 `App.tsx:101` 回到登录页；`1001/1000` → ≥5 s 后再连；连续 5 次 `1006` 后做一次 `api.me()`，401 则同上。
- 必须保留的三个易碎行为：① 用户侧**首次** open 不全量失效（`hadConnectionError`，`useUserEvents.ts:96-101`）；② 管理侧**每次** open 都 `all` 失效（`useAdminEvents.ts:103`）；③ 管理侧 250 ms 合并（`:99-101`）。
- 类型改名：`SseConnectionState` → `RealtimeConnectionState`、`useSseConnectionState` → `useRealtimeConnectionState`，7 个调用点是机械替换；`homeRefetchInterval` 签名不变，`HomePage.tsx:157` 一行都不用改。
- React Query 影响面：零结构变化。
- 测试策略：新建 `tests/support/fake-websocket.ts`，一次性替掉四份手写 `FakeEventSource`（提供 `addEventListener`/`send`/`close`/`readyState` 与静态 `OPEN`/`CLOSED`，`vi.stubGlobal("WebSocket", …)`）。新增断言：退避上界、隐藏标签页不重连、`1008` 不重连、重连后只做一次全量失效。

---

### 5. 迁移与兼容（Q6）

- **建议双轨一个版本**：先上 `/api/v1/ws`，保留两个 SSE 端点。前端优先 WS，仅在 `WebSocket` 不存在或连续 3 次握手失败时回退 SSE，并打一次 `console.warn`。代价是多维护一个版本；收益是反代配错时不必回滚服务端，只需改 nginx。之后单独一个任务删 SSE。
- 不做“服务端开关式灰度”：仓库没有 feature flag 设施，加一个 `LUX_WEB_REALTIME=ws|sse` 会长期滞留。
- **必须新增 ADR-054**：CLAUDE.md 明确“改核心架构先改 ADR”，而 ADR-030 把“SSE 协议循环留在 API 层”写成了边界条款（`docs/decisions/030-domain-module-boundaries.md:47`），ADR-049 把“不压缩”写成了 `text/event-stream` 特例（`:14,20,29`）——这两条不修订就直接违背 ADR。
- 文档清单：
  - `docs/API.md:157-158`（两份契约重写）+ `:53` 附近补 Origin/CSRF 说明
  - `docs/LUX-DEVELOPMENT.md:3950-4000`（LUX-153）、`:4048-4078`（LUX-187）、`:3241`、`:2983`、`:7530`
  - `docs/DEPLOYMENT.md:213-241`（补 Upgrade 头）；`:258` 的 Tailscale 检查项已含 WebSocket，措辞可收窄
  - `docs/decisions/030`、`049`、`017:18`（“只适合管理员 SSE”）
  - `docs/WEB-POLLING-AUDIT.md:26-27`
  - `docs/CHANGELOG.md`、`docs/COMPATIBILITY.md`、`docs/TEST-TARGETS.md`
  - `docs/PERFORMANCE.md` 追加新日期小节，**不改历史记录**（符合仓库惯例）
  - `CLAUDE.md`/`AGENTS.md` 无需改动（没有传输层表述）

---

### 6. 任务拆分（Q7）

| # | 范围 | 依赖 | 验收 | 改动文件 | 并行 |
|---|---|---|---|---|---|
| T0 | 写 ADR-054 并冻结信封/路径/Origin 策略（纯文档） | — | Owner 签字；`docs/API.md` 草稿两份契约 | `docs/decisions/054-*.md`、`docs/API.md` | 否（一切的前置） |
| T1 | 合并 LUX-891 | — | 分支合入 `test` | — | 外部 |
| T2 | 抽取 `src/api/websocket/{budget,shutdown,send}.rs`，`/embywebsocket` 改为引用 | T1 | `cargo test --locked --test emby emby_websocket::` 22/22 与 `--lib emby_websocket::` 9/9 保持；clippy 干净；零行为变更 | `src/api/websocket/*`、`src/api/emby_websocket.rs`、`src/api/legacy.rs`、`src/api/mod.rs` | 否 |
| T3 | `GET /api/v1/ws`：Origin 闸门、握手前鉴权、预算、关停、心跳、信封、频道选择 + 45 s 重校验 | T2 | 新增 `tests/admin/lux_websocket.rs`：401/403、跨源拒绝、双频道、`itemId` 收窄、超预算 503、关停 1001、撤销会话 1008 | `src/api/lux_websocket.rs`（新）、`src/api/admin/admin_logs_events.rs`、`src/api/legacy.rs`、`src/main.rs`、`tests/admin/main.rs`、`docs/TEST-TARGETS.md` | 否 |
| T4 | 前端 `lib/realtime/socket.ts` + 两个 hook 改造 + 类型改名 + 共享 Fake | T0（协议冻结即可，不等 T3） | `pnpm --dir web test`、`pnpm --dir web build`；三个保留行为各有测试 | `web/src/lib/realtime/*`、`useUserEvents.ts`、`useAdminEvents.ts`、7 个调用点、4 个测试文件 + 1 个 helper | **可并行 T3** |
| T5 | nginx 样例 + smoke 配置补 Upgrade 头；DEPLOYMENT/COMPATIBILITY/CHANGELOG | T3 | 用 `scripts/proxy-smoke.sh` 跑一次真实握手 | `docs/DEPLOYMENT.md`、`scripts/proxy-smoke.nginx.conf`、`docs/CHANGELOG.md`、`docs/COMPATIBILITY.md` | 与 T4 并行 |
| T6 | 删除两条 SSE 路由与其测试，收敛 ADR-049 措辞 | T4 发布一个版本后 | 旧端点 404；Web 测试与构建通过 | 同 T3 + 测试删除 | 否 |

**“第一个任务应该是抽取还是直接基于 LUX-891 实现”**：两个都不是。第一个任务必须是 T0（ADR + 协议冻结），因为它同时约束后端信封和前端 hook，改晚了的代价是双份重写；而且 CLAUDE.md 把 ADR 前置写成了硬约束。紧接着是 T2 抽取，而不是“照抄 LUX-891 的代码再实现”——抄一份意味着把**每用户上限、关停排空、写边界**三处安全敏感逻辑复制成两份并各自漂移，而抽取是纯 move，有 LUX-891 的 31 个测试兜底。唯一的反方意见是“只有一个消费者时抽取是过早抽象”，但第二个消费者已经由所有者决定，且就在下一个任务，不成立。

---

## 推荐与理由

**推荐方案 A（单端点 `/api/v1/ws` + `channel` 字段 + 服务端决定频道）**，理由：

1. 管理页现在每标签页 2 条流，而全局预算只有 64（`legacy.rs:1369`）。合并后减半，是这次迁移顺带能拿到的最大运维收益。
2. 一套 Origin 校验、一套预算、一套关停、一套前端 client——安全面和控制面都只有一处，符合 LUX-891 已经证明有效的结构。
3. 客户端零订阅消息，授权完全由握手时的服务端主体决定，不存在“客户端请求越权频道”的攻击面；降权只停投帧，不会把用户面一起关掉。
4. `scope` 字符串不变，两个 hook 的失效逻辑可以逐字保留，回归风险最小。

**任务顺序：T0 → T1 → T2 → (T3 ∥ T4 ∥ T5) → T6。**

**工作量（我的判断，未实测）**：T0 0.5 d、T2 0.5–1 d、T3 2–3 d、T4 1.5–2 d、T5 0.5 d、T6 0.5 d，合计约 **5.5–7.5 人日**，外加一个版本的双轨观察期。

**独立判断（Q8）——所有者已决定，但以下 6 点仍应提醒：**

1. **最可能的失败不是代码，而是文档里的 nginx 配置。** `docs/DEPLOYMENT.md:213-238` 和 `scripts/proxy-smoke.nginx.conf` 都缺 Upgrade 头。按这份配置部署的用户会得到 `close 1006`，然后回落到 30 s 轮询——**负载从“零轮询”变成“4 倍于断线基线”**，而这被记录为“功能正常”。这条必须在同一个 PR 里修掉，不能留到后续。
2. **WebSocket 让我们失去浏览器自带的重连。** `EventSource` 会自己重试且服务端可用 `retry:` 控制节奏；WebSocket 什么都不给。退避、抖动、隐藏标签页、断网、会话撤销全部变成我们的代码和我们的 bug。如果真实动机只是“不要两套传输”，那么**保留 SSE、删掉轮询兜底**能用更少的代码拿到同样的运维简化。这个替代方案值得在 ADR 里显式记录为“已评估”。
3. **合并通道会让一次反代故障同时打断用户面和管理面。** 两端口方案下管理流坏了不影响普通用户刷新。我仍然推荐合并，因为这个故障模式可观测且兜底是共用的，但所有者应当知道这是个主动取舍。
4. **如果要分阶段上线（两端口方案），应当先迁管理通道**：管理页的轮询最密（转码 2 s、四类任务列表各 5 s），爆炸半径最小（只有管理员），且它已经有一处更大的待修问题——`useAdminEvents` 从不暴露连接状态（`docs/WEB-POLLING-AUDIT.md:115`），所以管理端轮询**从来没有被关掉过**。先补连接状态比换传输协议更省事、更见效。
5. **不要原样迁移死代码。** `useUserEvents.ts` 里的 `libraryId` 收窄分支服务端从不发送；迁移时应删掉而不是移植，否则会把一个永远不会被覆盖的分支带进新协议。
6. **64 的连接预算本身就偏小**，迁移是修正它的唯一自然时机；如果换了 WS 却不动预算，单播设计在 30 个管理标签页下依然会 429，迁移的收益会被吃掉一半。

---

## 需要确认的问题

1. **路径**：新建 `/api/v1/ws`（可与 SSE 并存、可灰度回退），还是复用 `/api/v1/events`（路径更“连续”，但 handler 变成双模式）？
2. **通道划分**：单端点 + `channel` 字段（我的推荐），还是两个端点？这直接决定“能不能先只迁管理通道”。
3. **是否允许客户端上行消息**？如果长期只需要服务端推送，入站就只保留 ping，协议面最小；如果预期将来要做“客户端提交已读/进度”之类的消息，现在就把 `type` 路由留出来，否则以后要破坏性扩展。
4. **`LUX_ALLOWED_ORIGINS` 是否新增**？以及部署侧是否确认反向代理保留 `Host`（若不保留，严格同源校验会拒绝所有握手）。
5. **双轨时长**：接受“保留 SSE 一个版本再删”，还是直接硬切？硬切的话，反代配错就只能回滚镜像。
6. **连接预算**是否随本任务一起调整（256 全局 / 每用户 8 / 用户与管理分开）？还是严格保持 64 不动？
7. **是否顺带修复管理端连接状态**（`useAdminEvents` 暴露 state 并关闭 2 s/5 s 兜底轮询）？这是一个独立的、收益可能高于换协议的任务，我倾向于拆出去单独做，但它会影响 T4 的接口设计（是否顺带导出 `useRealtimeConnectionState` 给管理端）。
8. **ADR 编号**：`docs/decisions/` 当前最高是 `053`，我按 `054` 规划；如果 LUX-891 计划占用编号，请指定。
