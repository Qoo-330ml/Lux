# ADR-054：Lux Web 实时通道从 SSE 迁移到 WebSocket

## 状态

已采纳（项目所有者 2026-11-06 决定）

本文只定义合同，**不描述已实现的行为**：`GET /api/v1/ws` 在本文提交时尚不存在，两条 SSE 端点仍在
运行。实现顺序见「任务拆分与顺序」，`docs/API.md` 由 LUX-894 改写、由 LUX-897 删除 SSE 段落，
本任务不改动它。

## 日期

2026-11-06

## 背景与动机

Lux Web 目前有两条 SSE 端点，均来自 `src/api/admin/admin_logs_events.rs`：

| 端点 | handler | 鉴权 | 预算 | 首帧 | 心跳 |
|---|---|---|---|---|---|
| `GET /api/v1/events` | `user_events`（`:484-514`） | `require_web_user`（`src/api/users.rs:2813`，session cookie 或客户端令牌） | `state.sse_permits`（`:492`） | `event: ready` + `{"version":1}`（`:504`） | 15 s 注释行（`:521`） |
| `GET /api/v1/admin/events` | `admin_events`（`:414-466`） | `require_admin(.., false)`（`src/api/admin/admin_common.rs:3`） | 同一信号量（`:421`） | 同上（`:431`） | 15 s（`:438`） |

现状的四个问题：

1. **预算过小且不分类**。`MAX_SSE_CONNECTIONS = 64`（`src/api/legacy.rs:1369`）是两条流**共用**的一个
   信号量。管理页一条标签页同时持有 2 条流（`web/src/components/layout/LuxShell.tsx:48` 的用户流 +
   `web/src/features/admin/AdminLayout.tsx:32` 的管理流，`/admin` 挂在 `LuxShell` 下：
   `web/src/app/App.tsx:141`），所以约 32 个管理标签页就能让**所有用户**拿到 `429`，然后静默回落到
   30 秒轮询（`web/src/features/home/HomePage.tsx` 的 `homeRefetchInterval`，`query-keys.ts:122`）。
2. **管理端从不暴露连接状态**，管理页的兜底轮询（转码 2 s、四类任务列表各 5 s、仪表盘 15 s）从未
   按连接状态关闭（`docs/WEB-POLLING-AUDIT.md:27,115`）。
3. **同一语义两套传输**：浏览器 `EventSource` 自带重连，服务端可用 `retry:` 控节奏；但管理流没有连接
   状态、用户流与管理流各写一套首帧/心跳/降级逻辑，协议边界（ADR-030 的“SSE 协议循环留在 API 层”）
   也把 SSE 写成了长期条款。
4. **LUX-891 已经为 Emby 兼容的 `/embywebsocket` 打磨出**连接预算、关停联动、写边界、周期重校验四件
   事；Lux 自己的实时通道没有理由再写第二套。

盘点与逐条处理记录见 `docs/reviews/2026-11-06-sse-to-websocket-review.md`（本 ADR 的输入）。

## 项目所有者决定（不得改动，只能落实）

以下六条是所有者决定，本文其余部分只是把它们展开成可实施的合同；任何冲突以这六条为准。

1. Lux 自己的 Web 实时通道**从 SSE 全面迁移到 WebSocket**，端点为**单端点 `GET /api/v1/ws`**；同一条
   连接承载用户频道与管理员频道，由服务端按握手时的登录主体决定可接收哪些频道，**客户端不发订阅消息**。
2. **直接硬切**：同一个发布版本里删除 `GET /api/v1/events` 与 `GET /api/v1/admin/events`，不做双轨。
   前端在 WebSocket 不可用时仍保留现有的轮询兜底（这是今天 SSE 断开时的既有行为，不算 SSE 回退）。
3. **不新增 `LUX_ALLOWED_ORIGINS`**，只做严格同源校验。
4. **Emby 兼容的 `/embywebsocket`（LUX-891）继续保留**，协议不变，与 Lux 自己的 WebSocket **共用底层
   连接管理基础设施**（连接预算、关停联动、写边界、周期重校验），不复制代码。
5. 入站消息只保留心跳/ping，不开放客户端上行业务消息；信封沿用 `scope` 字符串逐字不变：
   `{"type":"ready","version":1}` 与
   `{"type":"invalidate","channel":"user|admin","scope":"…","itemId":"…"}`（`itemId` 可选）。不做断线续传。
6. 用户流与管理流**分开计数预算**并提高上限；具体数值由本 ADR 写出依据（现有
   `MAX_SSE_CONNECTIONS=64` 的来源、每标签页连接数、典型 NAS 并发）。

## 决定

编号 D1–D15 用于被取代条款的相互指认；旧 ADR 的标注只指向这里的编号，不复制正文。

- **D1 端点与频道**：唯一端点 `GET /api/v1/ws`（WebSocket Upgrade）。握手主体是用户 → 连接承载
  `user` 频道；主体另有服务器管理权限 → 同一连接**额外**承载 `admin` 频道。客户端不发送订阅/退订
  消息，服务端也不接受任何频道声明。合并成一条连接是主动取舍：一次反向代理故障会同时影响用户面与
  管理面（见「风险与回退」）。
- **D2 硬切**：新增端点与删除两条 SSE 路由发生在**同一个发布版本**内；不存在“先双轨一个版本”的
  观察期。删除时同步删除 `tests/admin/admin_events.rs` 中的流用例、`docs/API.md` 的两段契约与
  `docs/WEB-POLLING-AUDIT.md` 里的 SSE 描述。前端保留连接不可用时的既有多档轮询兜底（用户面 30 秒；
  管理面 2 s/5 s/15 s）——那是今天的既有行为，不是 SSE 回退。
- **D3 信封冻结**：下行只有两种帧，`ready` 与 `invalidate`；`scope` 的字符串值**逐字不变**
  （用户 `home|item|library|favorites|progress`，管理
  `all|dashboard|jobs|libraries|plugins|users|metadata|settings`）。不新增 `pong`、`subscribed`、
  `channel-revoked`、`error` 等下行帧；版本协商只发生在首帧的 `version`。
- **D4 入站最小化**：只接受心跳 `{"type":"ping"}`；未知 `type` 忽略（不关连接、不回错误帧）；
  二进制帧以 `1003` 关闭；不做断线续传，重连即全量失效。
- **D5 同源校验**：握手校验 `Origin`，策略见「握手与安全策略」；**不新增 `LUX_ALLOWED_ORIGINS`**；
  该判断**不得**使用 `X-Forwarded-Proto` 或任何其它转发头。
- **D6 握手认证**：只用 session cookie（`lux_session`，`HttpOnly; SameSite=Lax`）或客户端令牌，复用
  `require_web_user` / `require_admin(.., false)` 的现有判定；**禁止**把凭据放 query；WS 端点不要求
  也不接受 CSRF 令牌。
- **D7 频道授权**：管理频道只从服务端主体推导；周期重校验（沿用 LUX-891 的节奏常量）发现降权时
  **只停投 admin 帧、不关连接**；会话撤销/登出时以 `1008` 关闭整条连接。
- **D8 预算**：用户频道与管理频道各自一个独立预算实例，数值与推导见「资源与预算」；一条连接按其
  握手主体归入其中一个预算，**不重复计数**；超限在 Upgrade 之前返回 `429` + `Retry-After: 30`。
- **D9 共享基础设施**：`/api/v1/ws` 与 `/embywebsocket` 共用 `src/api/websocket/*`（预算、关停联动、
  写边界、周期重校验）。抽取先于实现（LUX-893），是纯 move，不复制代码、不允许两份逻辑各自漂移。
- **D10 关停**：进入关停时向所有实时连接发送 `1001`（going away）并有界等待排空，先于数据库关闭；
  `/embywebsocket` 与 `/api/v1/ws` 由同一个关停信号驱动。
- **D11 客户端退避**：指数退避 + 抖动，`1008` 不重连，隐藏标签页与离线暂停（约束见「关停与重连语义」）。
- **D12 反向代理**：nginx 必须补 `Upgrade`/`Connection` 头，且与 LUX-894 同批合入（见「反向代理要求」）。
- **D13 文档所有权**：`docs/API.md` 的 WS 契约由 LUX-894 写入，SSE 两段由 LUX-897 删除；`docs/CHANGELOG.md`
  条目属于引入行为变化的 LUX-894 与 LUX-897，本任务不写。
- **D14 不迁移死代码**：用户侧 `libraryId` 收窄分支（`web/src/features/activity/useUserEvents.ts:130`）
  服务端从不发送，迁移时删除而不是移植；`scope` 解析的其余分支逐字保留。
- **D15 保留三个易碎行为**：① 用户侧**首次** open 不全量失效（`hadConnectionError`，
  `useUserEvents.ts:96-101`）；② 管理侧**每次** open 都做 `all` 失效（`useAdminEvents.ts:103`）；
  ③ 管理侧 250 ms 合并窗（`useAdminEvents.ts:99-101`）。

## 已评估并否决的替代方案

### 保留 SSE，只删除轮询兜底

用更少的代码即可拿到同样的运维简化：不新增传输、不新增 Origin/退避代码，只把 `useAdminEvents` 的
连接状态导出给管理页并关掉 2 s/5 s 兜底。否决理由：所有者已决定迁移（决定 1、2）；且该方案保留了
“预算 64 共用”的根本问题——轮询关掉后 SSE 的 64 条上限依旧，超过上限的标签页仍然回落到轮询，收益
不放大反而更难观测。记录在案是为了将来出现“迁移收益不达预期”时，这是第一个应重新评估的选项。

### 两个端点（`/api/v1/ws` + `/api/v1/admin/ws`）

与今天 1:1 对应、鉴权各自独立、可以先只迁管理通道，爆炸半径最小。否决理由：管理页每标签页仍是 2 条
连接（用户流 + 管理流），预算压力不降；两套 handler 或一个带参数的 handler 都要各自处理 Origin 与
重校验，控制面翻倍。所有者选择了单端点（决定 1）。

### 沿用 `/api/v1/events`，同路径按 `Upgrade` 头分流

天然支持灰度回退，但同一路径语义变成“既是流又是升级”，`docs/API.md` 要写成双模式，测试要覆盖两条
路径，半年后删 SSE 时才简化。否决理由：与“直接硬切”（决定 2）直接冲突，且把过渡期复杂度写进了
长期路径语义。

### 双轨一个版本（WS 优先，握手失败回退 SSE）

能避免“反代配错只能回滚镜像”，代价是多维护一个版本的传输层、两套前端分支和两倍测试面，且仓库没有
feature flag 设施，回退判定（连续几次失败算失败）本身就是一个需要长期维护的启发式。否决理由：
所有者选择硬切（决定 2）；反代配错的兜底改为既有的轮询（用户面 30 秒，管理面 2 s/5 s/15 s），代价是
实时性下降而不是功能丢失。

### 新增 `LUX_ALLOWED_ORIGINS` 白名单

能容忍改写 `Host` 的反向代理，是盘点报告最初的建议。否决理由：所有者决定不新增配置项（决定 3）；
白名单把“谁能发起同源握手”变成运行时配置，配置错误会静默扩大信任面，而它要解决的场景（反代改写
`Host`）可以通过一行 `proxy_set_header Host $http_host;` 在部署侧修正。代价写进「风险与回退」：改写
`Host` 的代理会让握手被拒，前端落到轮询。

### 客户端发送订阅消息（`{"type":"subscribe","channels":["admin"]}`）

会让授权判定多一个客户端输入面，并引入“请求了但无权”的错误分支。否决理由：决定 1 要求频道完全由
服务端主体决定。

### 用 query 传 `?token=…` 完成握手认证

浏览器不能给 WebSocket 加自定义头，query 是最省事的替代。否决理由：凭据会进入访问日志、反代日志与
浏览器历史，违反 `CLAUDE.md` 的凭据不入日志要求；决定 6 已明确禁止。

### 补 `seq` 做断线续传

现有 SSE 从来没有续传语义（无 `id:`、无 `retry:`、前端不读 `Last-Event-ID`），所有帧都是幂等的
“去重读这个缓存”，重连时的全量失效已经覆盖正确性。补 `seq` 只增加测试面而不改变正确性；决定 5 已
明确不做。

### 用共享管理员 API Key 握手

`require_admin` 允许 `X-Lux-Api-Key`，但它没有用户主体，用户频道无从归属（`require_web_user` 对
该主体返回 401，`src/api/users.rs:2819-2826`）。否决理由：WS 端点只服务浏览器会话与 Emby 客户端
令牌；自动化继续走 JSON API。

## 协议与信封

### 端点

```
GET /api/v1/ws
Upgrade: websocket
Sec-WebSocket-Version: 13
```

- 只接受 `GET` + 合法 Upgrade；普通 GET 到该路径返回 `426 Upgrade Required`，由 handler 显式判断
  `Upgrade` 头实现（不依赖 Axum `WebSocketUpgrade` 提取器的默认 `400`），也不返回 200 空响应——避免
  被误当成健康检查。
- `wss:` 由反向代理终止 TLS 后提供；`luxd` 自身仍只监听明文 HTTP/1.1（`docs/DEPLOYMENT.md`）。
- 查询串不参与协议；服务端忽略全部查询参数，且**不**从中读取任何凭据。

### 下行（服务端 → 客户端）

首帧（连接建立后立即发送，一次连接只发一次）：

```json
{"type":"ready","version":1}
```

失效帧（用户频道）：

```json
{"type":"invalidate","channel":"user","scope":"home"}
{"type":"invalidate","channel":"user","scope":"progress","itemId":"a1b2c3d4"}
```

失效帧（管理频道）：

```json
{"type":"invalidate","channel":"admin","scope":"jobs"}
{"type":"invalidate","channel":"admin","scope":"all"}
```

约束：

- `channel` 只取 `user` / `admin`；`scope` 取值与今天逐字相同，前端现有 `parseUserEvent` /
  `parseScope` 的校验表原样保留，只在入口增加 `channel` 判定。
- `itemId` 可选，只在服务端本来就知道具体条目时出现（今天只有 `progress`）；长度上限沿用前端现有
  的 256 字符（`useUserEvents.ts:133`），超长视为非法帧、整帧丢弃。
- 丢帧（broadcast `Lagged`）时按现有语义降级：用户频道发 `home`，管理频道发 `all`；因为可能漏掉任意
  scope，降级帧不带 `itemId`。
- 帧不携带媒体库 ID、用户 ID、标题、路径或任何业务数据；它只通知“重新读取经过权限校验的 API”。
- 未知字段由客户端忽略；服务端不发送 `libraryId`（决定 D14）。

### 上行（客户端 → 服务端）

```json
{"type":"ping"}
```

- 只有这一种消息被识别；未知 `type` 忽略（不关连接、不回错误帧、不记日志正文）。
- 应用层 `ping` 只用于重置服务端空闲计时；服务端**不回** `pong` 帧（信封冻结，D3）。判活依赖协议级
  ping/pong 与 close 事件，见「心跳与空闲」。
- 二进制帧以 `1003`（unsupported data）关闭。
- 单帧/单消息上限 4 KiB；超限以 `1009`（message too big）关闭。4 KiB 的依据：唯一合法上行业务帧
  `{"type":"ping"}` 是 15 字节，4 KiB 留出约 270 倍余量给将来的带版本心跳，同时是 Emby 侧 8 KiB 上限
  的一半；常量随 `src/api/websocket/*` 一起定义（LUX-893/894）。

### 不做断线续传

重连后客户端做全量失效，与今天 `EventSource` 重连时的行为一致；服务端不提供 `seq`、`Last-Event-ID`
或重放缓冲。服务端可以在 `ready` 里加日志关联用的连接序号（不参与重放）；是否加由 LUX-894 决定，
加了也不改变本文的信封约束。

## 握手与安全策略

### 握手认证

- 两条凭据路径，判定完全复用现有函数，不新写一套：
  - **session cookie** `lux_session`（`HttpOnly; SameSite=Lax`，`src/api/admin/admin_common.rs:281-296`）
    → `require_web_user`（`src/api/users.rs:2813`）→ `require_web_principal` 的 cookie 分支；
  - **客户端令牌**（`X-Lux-Token` / `X-Emby-Token` / `X-MediaBrowser-Token` / `Authorization: Bearer`，
    `src/api/users.rs:3100-3114`）→ 同一函数的令牌分支。
- 管理频道资格额外调用 `require_admin(&headers, &state, false)`（不校验 CSRF，与今天的
  `admin_events` 一致）。
- **禁止**凭据出现在 query：`?token=`、`?api_key=` 一律不支持、不读取。
- 共享管理员 API Key（`X-Lux-Api-Key`）不适用于 WS 握手：`require_web_user` 对它返回 401，这是既有
  行为，本 ADR 不改变它。
- 握手失败在 Upgrade **之前**用普通 HTTP 响应返回：`401`（未认证）/`403`（无权限）/`429`（预算）
  /`400`（Origin 不允许，见下）。浏览器侧这些都会表现为 `close 1006`，所以服务端必须自己记日志，
  否则无法区分反代问题与鉴权问题。

### Origin 校验（严格同源）

原则：**只比较主机名与端口，不比较 scheme**。

1. 解析 `Origin`：只接受形如 `http://host[:port]` / `https://host[:port]` 的单一序列化源；带路径、
   带多个值、语法非法、或值为字面量 `null` 一律按“缺失”处理。
2. 规范化：host 转小写；端口为 scheme 默认端口（http 80、https 443）时视为不存在，其余端口保留。
   得到 `origin_authority`。
3. 解析请求的 `Host` 头（若缺失则回退到 URI authority），同样转小写、保留非默认端口，得到
   `host_authority`。
4. `origin_authority == host_authority` 才放行；不等 → `400`，不 Upgrade。
5. `Sec-Fetch-Site` 存在且等于 `cross-site` → 一律拒绝（廉价信号，不当作唯一控制）。缺失时不因此
   拒绝（老浏览器不一定发送）。
6. 缺失 `Origin`：**cookie 认证的握手拒绝**；仅当凭据是客户端令牌时放行（脚本、测试、非浏览器
   客户端）。判定方式是在调用 `require_web_user` 之前用现有的 `has_client_token(headers)` 记录凭据
   类别，两个分支最终仍走同一组认证函数。
7. 该判断**不读取** `X-Forwarded-Proto`、`Forwarded`、`X-Forwarded-Host`、`X-Real-IP` 或任何其它
   转发头。`CLAUDE.md` 明确 `X-Forwarded-Proto` 只是展示/传输元数据，不得用于认证、授权或 ACL；
   按盘点报告最初建议“scheme 取 `X-Forwarded-Proto`”会直接违反这条规则，因此**该建议作废**。

**为什么不比较 scheme**：

- `luxd` 只提供明文 HTTP（`docs/DEPLOYMENT.md`），浏览器侧是 `https`/`wss`，上游是 `http`。要比较
  scheme 就必须引入 `X-Forwarded-Proto` 或部署侧配置，前者被 `CLAUDE.md` 禁止，后者被决定 3 排除。
- 同源策略的实际边界是“哪个页面能以用户的 cookie 连上来”，而 cookie 的 `SameSite=Lax` 已经让跨站
  握手不带 cookie；scheme 不是这条边界的组成部分。
- 浏览器在 `https` 页面里本来就禁止 `ws://`（混合内容），所以“https 页面连明文 WS”这条路径不成立。

**残余风险**（明确记录，不掩盖）：

- 若部署本身跑在明文 HTTP 上，同主机名的其它明文页面可以发起同源握手；但那时攻击者已经能读取或
  篡改同一链路上的明文流量，Origin 校验不是这一层的防线。
- 若反向代理改写了 `Host`（例如把 `Host` 改成上游 `127.0.0.1:8097`），`Origin` 与 `Host` 不再一致，
  **所有握手都会被拒绝**，前端自动落到既有轮询（用户面 30 秒）。这是决定 3（不加配置项）的直接
  后果，也是已知局限；修正方式在部署侧（`proxy_set_header Host $http_host;`），不是放宽校验。
- 非默认端口的部署必须让反代保留端口（`$http_host` 而不是 `$host`），否则同样被拒。LUX-896 的部署
  文档必须写明这一条。

**未来放宽的唯一路径**：新增 ADR 修订（新编号或 ADR-054 的正式修订记录），重新评估白名单或
`Host` 改写容忍策略；不允许以“实现方便”为理由在 LUX-894 里放宽本节的任何一条。

### CSRF 等价防护

WS 握手是 `GET`，带不了 `X-CSRF-Token`，所以本端点**不要求也不接受** CSRF 令牌；等价防护 =
`SameSite=Lax` + 上面的 Origin 校验。`SameSite=Lax` 挡住绝大多数跨站握手（跨站请求不带 Lax cookie），
但它不能替代 Origin 校验：浏览器行为存在差异，且一旦将来把 cookie 改成 `SameSite=None`，Origin 校验
就是唯一防线。`docs/API.md` 的 WS 段落必须写明“不要求也不接受 CSRF 令牌，改由 Origin 校验”。

### 管理员频道授权与周期重校验

- 资格只在握手时算一次，之后按 45 秒周期重校验（`WEBSOCKET_DEFAULT_REVALIDATE_INTERVAL`，见「资源与
  预算」的来源说明）。重校验复用握手时的同一组函数（含远程访问与设备/客户端规则），帧投递只读这次
  结果。
- **与 LUX-891 的一处有意差异**：Emby 端点在每次推送前都重校验（推送由客户端的 `SessionsStart` 节奏
  驱动，最快 1 s 一次）；Lux 的失效帧速率跟随业务事件（任务进度、播放进度），每帧查一次会话会把数据库
  查询次数直接绑到事件速率上，因此只按 45 秒计时器重校验。代价是撤销后最长一个周期的窗口（见「风险与
  回退」第 6 条）。
- 会话消失、令牌失效或远程访问规则不再通过 → `1008` 关闭整条连接；仅仅是**降权**
  （`can_manage_server` 变 false）→ 只停投 `admin` 帧，连接与 `user` 帧继续。
- 不做 `admin` 频道恢复：重校验发现权限回来后重新开始投递即可，不需要客户端状态，也不下发“已恢复”
  帧（D3）。
- 今天的 SSE 从不重校验，连接活到进程结束；`1008` 关闭比现状更严格，不是放松。

### 入站输入与日志

- 入站一律不可信：只按字节解析 JSON，不按客户端提供的 ID 查库，不做任何基于客户端字符串的查询。
- 日志只记 `user_id`、连接序号、close code、`channel`（以及预算类别）；**不记** cookie、token、
  query、完整 URI（只记 path）。`Origin` 只在 `debug` 级记录，并且它本身会泄露内网主机名，默认级别
  下不出现。握手被拒时记录拒绝原因分类（`origin-missing` / `origin-mismatch` / `sec-fetch-site` /
  `unauthenticated` / `budget`），不记录头的原值。

## 资源与预算

### 现有 64 的来源

`MAX_SSE_CONNECTIONS = 64`（`src/api/legacy.rs:1369`）的注释写明了它的来历：
“Each stream holds a task and a 16 KiB buffer, so the cap keeps them bounded.”
16 KiB 就是 `tokio::io::duplex(16 * 1024)`（`src/api/admin/admin_logs_events.rs:426,499`），
即 64 × 16 KiB = 1 MiB 固定缓冲 + 64 个任务。它是**内存上界**推出来的数字，不是实测的并发需求；
两条流共用同一信号量（`SseConnectionBudget`，`legacy.rs:165-177`），超限返回 `429` +
`Retry-After: 30`（`legacy.rs:1811-1823`，被 `admin_logs_events.rs:421,492` 调用）。

### 每标签页连接数（今天）

| 场景 | 连接数 | 依据 |
|---|---|---|
| 普通用户标签页 | 1（用户流） | `LuxShell.tsx:48` → `useUserEvents()` |
| 管理页标签页 | 2（用户流 + 管理流） | `/admin` 挂在 `LuxShell` 下（`App.tsx:141`）+ `AdminLayout.tsx:32` |
| 播放页 | 0（既不在 `LuxShell` 内，也不开管理流） | `App.tsx:139` 的 `watch/:itemId` 在 `LuxShell` 之外 |

因此 64 的共用预算在**约 32 个管理标签页**时被耗尽，之后所有人落到轮询。

### 典型 NAS 并发（假设，必须由 LUX-894 用现场数据复核）

Lux 的部署定位是单进程 + SQLite 的家庭 NAS（ADR-001、ADR-002），使用者是少量账号、每人多个标签页。
下表是本文取值用到的全部假设；它们不是实测数据，LUX-894 落地后必须以真实部署的并发连接数与
`429` 计数复核，若与假设偏差超过 2 倍，按 D8 的同一套推导重算并修订本文。

| 假设 | 取值 | 依据 | 置信度 |
|---|---|---|---|
| 账号数 | ≤ 8 | 单机家庭部署；现有 64 预算在正常使用下未被报告为瓶颈 | 假设 |
| 每账号同时打开标签页 | ≤ 4 | 首页 / 媒体库 / 详情 / 管理页的常见路径；每标签页 1 条用户流（播放页不建流，见上表） | 假设 |
| 重连重叠系数 | 2 | 退避重连期间旧连接最多保留一个服务端空闲周期（120 s，见下） | 推导 |
| 管理员账号数 | ≤ 3 | 同上 | 假设 |
| 管理员标签页 | ≤ 4 | 仪表盘常驻 + 操作页 | 假设 |

### 取值与推导

| 预算 | 取值 | 推导 |
|---|---|---|
| 用户频道总预算 | **128** | 8 账号 × 4 标签页 × 2（重连重叠）= 64 → 取 2 倍余量给脚本、测试与未计入设备 |
| 管理频道总预算 | **64** | 3 管理员 × 4 标签页 × 2 = 24 → 取约 2.7 倍余量 |
| 用户频道每用户上限 | **16** | 4 标签页 × 2 设备（同一账号的手机 + 电脑）= 8 条在线，× 2 重连重叠 |
| 管理频道每用户上限 | **8** | 4 标签页 × 2 = 8（管理员通常只在桌面端常驻） |

补充说明：

- 新总量 128 + 64 = 192，是旧上限 64 的 3 倍。常态占用按每条连接 16 KiB 量级估算约 3 MiB，与今天的
  1 MiB 同一数量级。这里的 16 KiB 不是新估算：LUX-891 的写边界是
  `WEBSOCKET_DEFAULT_WRITE_BUFFER_BYTES = 16 * 1024`（初始容量提示）与
  `WEBSOCKET_DEFAULT_MAX_WRITE_BUFFER_BYTES = 4 * 1024 * 1024`（硬上限），Lux 的失效帧只有几十字节，缓冲
  不会长到初始容量以上；4 MiB/条只是“对端停止读取”时的结构上界（192 × 4 MiB = 768 MiB 是理论上界，
  不是预期占用），而 `WEBSOCKET_DEFAULT_SEND_TIMEOUT = 10 s` 的发送超时会把实际增长限制在 10 秒内产生的
  帧上并关闭连接。**该超时与上限的实测值待 LUX-894 记录**（RSS 差值，本机 arm64）。
- 一条连接只占一个预算名额，按握手主体归类：`can_manage_server` 为真 → 计入管理预算，否则计入用户
  预算（管理员的用户频道帧仍走同一条连接，不额外占名额）。降权后**不迁移**名额，只停投 admin 帧，
  避免计数与连接生命周期错位。代价：管理员标签页不占用户预算，普通用户的突发也不会挤掉管理员，反之
  亦然——这正是“分开计数”的目的。
- 每用户上限按**用户**而不是按连接序号计数，检查发生在握手阶段；已有连接不受新握手被拒的影响。
- 两组数字自洽：用户 128 = 8 账号 × 每用户 16；管理 64 = 8 个管理员 × 每用户 8（假设里只有 3 个管理员
  账号）→ 只有“所有人同时跑满”才会触到总上限，正常情况下每用户上限先于总上限生效，便于定位是哪一类
  客户端在放大连接数。
- 这些数值是推导值，不是实测值：LUX-894 的验收要求记录一次真实部署的峰值连接数与 `429` 次数；
  若峰值超过上限的 50%，按同一推导上调，并在本文补测点与结论。

### 超限行为

- 超限在 Upgrade **之前**返回 `429` + `Retry-After: 30`（沿用今天 `sse_capacity_response` 的响应
  形状与文案），不 Upgrade、不关闭已有连接。
- 前端把 429 当作普通握手失败处理：按 D11 的退避重连，连接不可用期间使用既有轮询兜底。不新增
  “预算耗尽”专用状态或提示。

### 心跳与空闲

| 参数 | 取值 | 来源 |
|---|---|---|
| 服务端协议级 ping | 25 s | `WEBSOCKET_DEFAULT_HEARTBEAT_INTERVAL`（`src/api/websocket/mod.rs`，LUX-893 抽取） |
| 空闲关闭 | 120 s 无任何入站帧（含 pong） | `WEBSOCKET_DEFAULT_IDLE_TIMEOUT`（同文件） |
| 周期重校验 | 45 s | `WEBSOCKET_DEFAULT_REVALIDATE_INTERVAL`（同文件） |
| 关停排空 | 5 s | `EMBY_WEBSOCKET_SHUTDOWN_DRAIN_TIMEOUT`（`src/api/emby_websocket.rs`；`wait_until_idle` 在 `src/api/websocket/budget.rs`） |

上表的常量读自本地 `tspace/emby-websocket` 分支（LUX-891 仍在返工，尚未合并到 `test`，所以当前快照里
没有 `src/api/emby_websocket.rs`）。LUX-893 抽取时以**合并后**的常量为准；若数值发生变化必须同步修订
本节，并在 `docs/LUX-DEVELOPMENT.md` 的 LUX-893 结果里记录最终值。LUX-893 已按合并后的常量核对：四个
数值均未变，前三个随共享基础设施移到 `src/api/websocket/mod.rs`（`WEBSOCKET_DEFAULT_*`），排空上限仍
留在 Emby 模块（它是 `/embywebsocket` 关停的配置，不是共享类型的字段）。

浏览器对协议级 ping **自动回 pong**（RFC 6455 §5.5.2），所以“120 s 无入站帧”在正常浏览器上是
120 s 无 pong，而不是 120 s 无用户消息。LUX-891 的该常量注释写的就是“连 pong 都不发才算空闲”，
LUX-894 必须保持同一语义并用真实浏览器验证：若把 pong 排除在空闲计时之外，每条连接都会在 120 s 后
被误杀。

JS 看不见协议级 ping，客户端不能据此判活；判活只依赖 `close`/`error` 事件与写入失败。半开 TCP 由
服务端空闲计时兜底（≤ 120 s 后关闭），客户端收到 close 再退避重连——这与今天 SSE 的心跳行为等价
（SSE 的注释心跳同样不会触发浏览器超时）。

## 关停与重连语义

### 服务端关停

- 关停开始时（`ReadyAppState::shutdown` 链，`src/api/legacy.rs` 现有顺序）先置位共享的关停
  `watch`，两个 WebSocket 端点（`/embywebsocket`、`/api/v1/ws`）同时收到：停止接受新握手
  （`503`），向每条连接发 `1001`（going away），然后**有界等待**排空 5 秒
  （`EMBY_WEBSOCKET_SHUTDOWN_DRAIN_TIMEOUT`，`src/api/emby_websocket.rs`；由共享的
  `wait_until_idle(timeout)` 执行）。
- 关停排空必须发生在数据库关闭与事件 hub drop 之前，否则连接会在没有 close 帧的情况下被切断，
  客户端只能看到 `1006` 并做一次全量失效（功能上可接受，但日志里会与反代故障混淆）。
- 关停期间不再投递 `invalidate` 帧；已排队的帧直接丢弃，客户端重连后的全量失效会补齐。

### 客户端退避（LUX-895 的约束）

以下数值取自盘点报告 §4 的建议，作为 LUX-895 的起点与上限约束；它们不是实测结果。

- 指数退避：`min(30 s, 500 ms × 2^n)`，叠加 ±20% 抖动；**只有收到首帧 `ready` 才把 `n` 归零**，
  连接建立但迟迟没有 `ready` 视为失败。
- `1008`（策略违规 / 会话撤销）：**不重连**；主动失效 `queryKeys.me`，让 `web/src/app/App.tsx`
  回到登录页。
- `1001` / `1000`：至少等 5 s 再连（服务端可能在重启）。
- 连续 5 次 `1006`：做一次 `api.me()`，`401` 则按会话失效处理（同上），否则继续退避。之所以需要
  这一步：反代故障与 401 在浏览器侧都是 `1006`（见「反向代理要求」）。
- 页面可见性与网络：`document.hidden` 时暂停重连，`visibilitychange` 转可见时立即重试一次；`offline`
  事件暂停，`online` 事件重试。合盖的笔记本不能持续累积定时器。
- 上述常数是约束而不是最终值：LUX-895 可以在 500 ms–30 s 的上下界内调整基数与上限，抖动幅度可在
  10%–20% 之间选择，并记录理由；但不能去掉抖动、不能对 `1008` 重连、不能在隐藏标签页继续重连。

### 重连后的失效范围

- 用户侧：只有**曾经断线**的连接在重新 open 时做一次 `home` 全量失效（D15①）；首次连接不做。
- 管理侧：每次 open 都做 `all` 失效（D15②），并以 250 ms 合并窗聚合（D15③）。
- 不做续传，因此不需要“补发漏帧”；重连即全量，与今天一致。

## 反向代理要求

- nginx 必须为实时路径补两行，缺一不可：

  ```nginx
  proxy_set_header Upgrade $http_upgrade;
  proxy_set_header Connection "upgrade";
  ```

  以及保留 `proxy_http_version 1.1;`、`proxy_set_header Host $http_host;`（见下）、关闭响应缓冲
  （`proxy_buffering off;`，样例已有）。
- **缺失时的表现**：`Upgrade` 头被剥掉后请求以普通 GET 到达，Axum 的 `WebSocketUpgrade` 提取器直接
  `400`，浏览器侧只看到 `close 1006`——**与 401 完全无法区分**。前端随后落到既有轮询（用户面 30 秒，
  管理面 2 s/5 s/15 s），管理页负载反而高于断线基线，而 UI 表现为“功能正常”。这是本次迁移最可能的
  失败点，也是 LUX-896 必须与 LUX-894 **同批合入**的原因。
- `Host` 必须是浏览器看到的主机名（`$http_host` 保留端口；`$host` 会丢掉非默认端口）。写成
  `$host` 时，非默认端口部署一律握手失败。
- `proxy_read_timeout` 必须大于心跳间隔（样例的 1 h 远大于 25 s，足够）；nginx 的 `http2 on` 只影响
  浏览器到 nginx 的一段，上游仍是 `proxy_http_version 1.1` 的经典 Upgrade，不需要 RFC 8441。
- Caddy 的 `reverse_proxy` 默认支持升级；仓库里没有 Caddy 配置，`docs/COMPATIBILITY.md` 已注明该链路
  未实机验证，LUX-896 不改这一结论。
- 中间件侧不拦截升级：压缩谓词只压成功的 `application/json`（`src/api/routes.rs:103-120`），ETag
  中间件对非 2xx 直接返回（`routes.rs:26`），`101` 响应不进入压缩或缓冲路径（ADR-049 的谓词不匹配它）。

## 与 Emby `/embywebsocket` 的关系

- `/embywebsocket` 保留，**协议不变**（LUX-891）。它承载 Emby 客户端的 `StreamState` 轮询计划、
  Emby 专有的消息处理与关闭语义；本 ADR 不改变其中任何一条。
- 共用的是底层基础设施，抽到 `src/api/websocket/{mod,budget,shutdown,send}.rs`（LUX-893）：
  全局/每用户连接计数、关停 `watch`、有界写与 close 帧构造、周期重校验的节奏常量。两边的差异只保留
  在各自的顶层循环里：Emby 的唤醒源是 `StreamState` 计划，Lux 的是 `broadcast::Receiver`。
  LUX-893 已落地：类型改为中性名（`WebsocketConnections` / `WebsocketPermit` / `WebsocketShutdown` /
  `WebsocketLimits`），`EmbyWebsocketLimits` 保留为同一个 limits 类型的公开别名，Emby 模块只剩协议
  与循环代码。
- 抽取是纯 move，验收标准是 LUX-891 的集成测试与单测全部保持通过（合并后实际为 23 + 10 例，见盘点报告），
  clippy 干净，零行为变更。抽取必须先于 LUX-894（依赖 LUX-891 已合并，避免在四个文件的同一区域反复
  冲突：`src/api/legacy.rs` 的 `CoreState` 字段与 builder、`src/api/mod.rs` 的 `pub use`、
  `src/main.rs` 的两处关停调用、`Cargo.toml`）。
- 预算实例分开：Emby 继续用今天的 32 全局 / 4 每用户（LUX-893 起是 limits 类型的默认值
  `WEBSOCKET_DEFAULT_MAX_CONNECTIONS` / `WEBSOCKET_DEFAULT_MAX_CONNECTIONS_PER_USER`，注入给 Emby 自己的
  实例），Lux 的 user/admin 各自一个实例；三者互不挤占，但共享
  同一套类型、可注入的 limits 结构（`WebsocketLimits`，`EmbyWebsocketLimits` 是它的别名，测试可缩小
  上限）与关停信号。
- 协议细节保持两边独立：`/embywebsocket` 继续接受 Emby 客户端习惯的 `?api_key=` 与 `deviceId=` 查询
  参数（那是 Emby 兼容面，LUX-891 已固定），`/api/v1/ws` **不接受**任何 query 凭据（D6）。同一个文件里
  的两条路径在这一点上刻意不同，不要互相“对齐”。
- close code：LUX-893 抽取时把 `1000`/`1001`/`1008` 常量一起移动；Lux 端点额外用 `1003`（二进制）与
  `1009`（超限），这两个常量属于 Lux handler，不进 Emby 的闭集。

## 兼容性、文档与 API 合同的所有权

| 文档 | 谁改 | 改什么 |
|---|---|---|
| `docs/API.md` | LUX-894 / LUX-897 | 894 写 WS 契约（含“不要求 CSRF、改由 Origin 校验”）；897 删除两条 SSE 段落 |
| `docs/DEPLOYMENT.md`、`scripts/proxy-smoke.nginx.conf` | LUX-896 | 补 `Upgrade`/`Connection`、`Host $http_host`；说明缺失时的 `1006` 表现 |
| `docs/CHANGELOG.md` | LUX-894 / LUX-897 | 各自的实际行为变化条目；LUX-892 不写 |
| `docs/COMPATIBILITY.md` | LUX-896 | 记录真实反代握手验证结论（含未验证项） |
| `docs/WEB-POLLING-AUDIT.md` | LUX-897 / LUX-898 | 897 改 SSE 描述；898 关闭管理端兜底轮询后更新 |
| `docs/TEST-TARGETS.md` | LUX-894 / LUX-897 | 新增 `lux_websocket` 目标；删除 SSE 用例计数 |
| ADR-030 / 049 / 017 | LUX-892（本任务） | 只加“已被 ADR-054 部分取代”的状态标注，正文不动 |

## 任务拆分与顺序

顺序：**LUX-892（本文）→ LUX-893 → LUX-894（+ LUX-896 同批）→ LUX-895 → LUX-897**；LUX-898 独立。
LUX-894/895/897 属于同一个发布版本（决定 2），因此 893 之后的部分必须在同一发布列车里完成，不能
“先上 WS 观察一个版本再删 SSE”。

| # | 范围 | 依赖 | 关键验收 | 同批约束 |
|---|---|---|---|---|
| LUX-892 | 本文 + 旧 ADR 标注 + `docs/LUX-DEVELOPMENT.md` 任务条目 | — | 只改 `docs/`；所有者六条决定逐条落实 | — |
| LUX-893 | 抽取 `src/api/websocket/{budget,shutdown,send,mod}.rs`，`emby_websocket.rs` 改为引用 | LUX-891 合并 | LUX-891 的 22 + 9 例保持通过；clippy 干净；零行为变更 | 先行；不做功能改动 |
| LUX-894 | `GET /api/v1/ws`：Origin 闸门、握手前认证、双频道、双预算、关停、心跳、45 s 重校验、信封 | LUX-893 | 401/403/400（Origin）/429、双频道、降权只停 admin 帧、撤销 `1008`、关停 `1001`、4 KiB 上限、二进制 `1003` | 与 LUX-896 同批合入 |
| LUX-895 | `web/src/lib/realtime/*`、两个 hook 改造、类型改名、共享 Fake WebSocket | 协议冻结（本文） | 退避上界、`1008` 不重连、隐藏标签页不重连、三个易碎行为各有测试 | 与 LUX-894 同发布 |
| LUX-896 | 反代配置与部署文档（`docs/DEPLOYMENT.md`、`scripts/proxy-smoke.nginx.conf`） | LUX-894 | 真实握手 smoke 通过；缺头时文档描述与实测一致 | **必须与 LUX-894 同批** |
| LUX-897 | 删除两条 SSE 路由、旧测试与相关文档段落 | LUX-894 + LUX-895 | 旧端点 404；Web 测试与构建通过；无残留引用 | 同一发布版本 |
| LUX-898 | 管理端暴露实时连接状态并关闭兜底轮询 | 无（不依赖迁移） | 管理页按连接状态关闭 2 s/5 s/15 s 轮询；断线恢复轮询 | 可任意时点，独立发布 |

LUX-898 单独拆出的理由：它的收益（管理页每次打开省掉 2 s 转码轮询与四类 5 s 任务轮询）**可能大于**
换传输协议本身，而且它不依赖 WS 是否上线；把它绑进迁移只会让迁移的验收面变模糊。

## 风险与回退

1. **硬切的回退只有镜像回滚**。反代配错时，WS 握手失败 → 前端落到轮询，功能不丢但实时性下降；由于
   前端随镜像发布，无法只回退前端或只回退后端。回滚到上一个镜像会同时恢复两条 SSE 端点。
2. **合并通道放大故障域**：一次反代/Origin 判定故障会同时打断用户面与管理面的实时刷新。可观测性靠
   握手拒绝原因的日志分类；兜底轮询是两端共用的。
3. **重连逻辑是我们自己的代码**。`EventSource` 自带重连，WebSocket 什么都不给；退避、抖动、隐藏
   标签页、断网、会话撤销全部新增，任何一处写错都会表现为重连风暴或“登录了但页面不刷新”。D11 与
   D15 的可测条目是唯一的防线。
4. **严格 Origin + `Host` 改写 = 握手被拒**。这是决定 3 的直接后果，已写入「握手与安全策略」的已知
   局限；修正路径在部署侧，放宽必须走新的 ADR 修订。
5. **预算数值是推导值**。超限的表现是 `429` + 轮询（可见、不丢数据），不是静默降级；但若真实部署
   的并发远超假设（例如脚本大量建连），需要按 D8 重算并补测点。
6. **会话撤销有最长一个重校验周期（45 s，待 LUX-894 实测确认）的窗口**：期间连接仍可能收到
   `invalidate` 帧，但帧内不含业务数据，且现状 SSE 从不重校验，这一条是收紧而不是放松。
7. **`Sec-Fetch-Site` 不是所有客户端都发送**，Origin 校验才是主控制；反过来，若将来某个客户端发送
   了错误的 `Sec-Fetch-Site`，它会被拒绝且没有配置逃生口。
8. **没有断线续传**（与现状一致）；重连即全量失效，首页与详情页会各多一次请求。
9. **本机为 ARM64**，没有 NAS/x86 并发数据，也没有在真实反向代理上做过握手验证（属于 LUX-896）；
   本文的所有容量结论都只到“推导 + 待复核”的程度。

## 后果

- 一个端点、一套 Origin 校验、一套预算、一套关停、一套前端客户端；安全面与控制面各只有一处。
- 管理页每标签页的连接数从 2 降到 1，且用户/管理预算分离后不再互相挤占。
- 浏览器不再受“每源约 6 条 HTTP/1.1 连接”的约束（WebSocket 升级后脱离该连接池；该行为来自盘点报告
  §3，本次未实测）。
- 代价是新增了一段必须自己维护的重连与可见性逻辑，以及“反代必须配对”的部署前置条件。
- ADR-030、ADR-049、ADR-017 中受影响的句子按本 ADR 标注为部分取代；其余内容不变。
