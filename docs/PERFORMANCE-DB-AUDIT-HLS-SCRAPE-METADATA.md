# HLS 转码 / 刮削（TMDb）/ 元数据写入的数据库行为审计

日期：2026-10-31。基线：`test` 分支（含第三轮任务 A 的合并成果）。
范围：`src/` 内 HLS 转码、刮削（TMDb）、元数据写入三条路径的 SQL 行为。**只读审计，本文件不修改任何 `src` 代码。**
来源：交接清单 `docs/PERFORMANCE-DB-HANDOFF.md` 第 7 节最后一条「HLS 转码、刮削、元数据写入的数据库行为：第二轮没有覆盖，值得单独审计」。

## 0. 怎么读这份文件

### 0.1 证据分级

每条发现都标了证据等级：

- **已读代码确认**：语句形状、事务边界、调用位置都是从当前源码读出来的，行号可复查。
- **推测**：频率、放大倍数或运行期成本依赖客户端行为、数据分布或磁盘特性，没有本机采样支撑。

**本文件没有任何运行时数字。** 所有"每次 X 条语句"是静态计数（对着代码数 `query/query_as/query_scalar/execute/begin`），
不是采样结果；所有"每次播放 N 次提交"里，分片间隔取自代码常量（`HLS_SEGMENT_DURATION_TICKS = 4 s`，
`src/application/playback/hls.rs:36`），清单刷新次数按 hls.js / Emby 客户端的标准行为**推测**。
行号是 2026-10-31 的快照，后续重构会偏移，请用函数名重新定位。

**简写约定**：为节省表格宽度，正文里的短路径按下表解析；`metadata.rs:` 一律指 `src/application/metadata.rs`，
`src/storage/metadata.rs` 始终写全路径。

| 简写 | 实际路径 |
|---|---|
| `session.rs` | `src/application/playback/session.rs` |
| `sessions.rs` | `src/storage/sessions.rs` |
| `hls.rs` | `src/application/playback/hls.rs` |
| `candidates.rs` | `src/application/candidates.rs` |
| `reidentify.rs` | `src/application/reidentify.rs` |
| `relations.rs` | `src/application/people/relations.rs` |
| `people.rs` | `src/storage/people.rs` |
| `catalog.rs` | `src/storage/catalog.rs` |
| `images.rs` | `src/application/images.rs` |
| `nfo.rs` | `src/application/nfo.rs` |
| `scraper.rs` | `src/application/scraper.rs` |
| `users.rs` | `src/storage/users.rs` |
| `repository.rs` | `src/storage/repository.rs` |
| `database_cleanup.rs` | `src/storage/database_cleanup.rs` |
| `service.rs` | `src/application/people/service.rs` |

### 0.2 分类沿用交接清单第 5 节

与第 5 节（W1–W5 写入侧）同一套口径：N+1 逐条查询/写入、缺失事务导致多次提交、热路径上的全表/大范围扫描、
缺索引的 WHERE/ORDER BY、重复读取同一行、写放大（无变化也 UPDATE）。编号前缀：

- `H*`：HLS 转码路径
- `S*`：刮削（TMDb）路径
- `M*`：元数据写入路径
- `I*`：缺失索引（跨三条路径）

### 0.3 本机环境限制

审计在 macOS `arm64` 开发机上完成，**没有 NAS（机械盘 / 低功耗 x86）测量**。凡涉及 fsync 成本、
写放大在慢盘上的绝对影响，都只能按"每次提交一次 fsync"的量级外推，不能当作 NAS 结论。

## 1. 结论摘要

| 编号 | 问题（一句话） | 位置 | 触发频率（静态/推测） | 影响 | 价值 |
|---|---|---|---|---|---|
| **H1** | 每个 HLS 分片/清单 GET 都跑一次含 5 条语句的 `BEGIN IMMEDIATE` 写事务 | `src/api/playback.rs:4397`、`src/storage/sessions.rs:275` | 每路播放约 30 次/分（分片 4 s + 清单刷新） | 播放期间持续写库，`synchronous=FULL` 下每次提交一次 fsync | **高** |
| **H2** | 每个分片请求读 6 次库，其中 session 行读 2 次、`users` 行读 2 次、`server_settings` 每次读 | `src/application/playback/session.rs:890`、`src/api/legacy.rs:1802` | 同上 | 读放大 + 解析开销；与 H1 叠加 | **高** |
| **M2** | 演员落库按"每个演员一个写事务"，上限 100 演员 | `src/application/people/relations.rs:516`、`src/storage/people.rs:612` | 每条目最多 ~100–200 次提交 | 元数据写入侧最大的提交放大 | **高** |
| **M4** | NFO 未变更的每次扫描仍重写 `collection_item_references`（DELETE+INSERT） | `src/application/metadata.rs:3471`、`src/storage/catalog.rs:7691` | 每个 MOVIE 每次复扫 1 次写事务 | 全库复扫时与扫描写路径抢同一写锁 | **高** |
| **S1** | 刮削候选循环内每个候选写一次能力表（各自一个事务 + 写锁） | `src/application/candidates.rs:831`、`:1105` | 手动搜索最多 20 候选 → 最多 ~40 次提交 | 用户点一次"搜索候选"就产生一串提交 | **高** |
| **M3** | 元数据选中无条件 `UPDATE media_items`（无"值未变则不写"） | `src/storage/metadata.rs:1742` | 每次选中/回填刷新 | 每次写触发 `catalog_outbox` 最多 5 行（见 `docs/INDEX-AUDIT.md` §7） | **中-高** |
| **M1** | 应用选中候选的落库由 4 组独立事务组成，没有总事务 | `src/application/candidates.rs:2989` | 每条目 1 次 | 中途失败留下"图片/演员已写、候选仍 PENDING"的部分状态 | **中** |
| **M5** | 每张图片 3 个写事务（claim → 图片行 → finish），全部串行在元数据写锁上 | `src/application/images.rs:528`、`:549` | 每条目 ~3–8 张图 → ~10–24 次提交 | 图片是刮削写放大第二来源 | **中** |
| **S5** | 过期 PENDING 候选清理缺 `(status, expires_at)` 索引 | `src/storage/database_cleanup.rs:929` | 每批清理 | 候选积压时变成排序大范围扫描 | **中** |
| **S6** | `jobs` SSE 事件每秒失效 10 个管理端查询键，其中"元数据任务列表"带 `pending_counts` 聚合 | `web/src/features/admin/useAdminEvents.ts:18`、`src/storage/jobs/metadata_reidentify/lifecycle.rs:186` | 任务运行时 ≤1 次/秒（页面打开时） | 管理端轮询放大；聚合按 job_items × candidates 规模增长 | **中** |
| **H5** | 管理端转码页每 2 s 轮询，N 个会话 → 1+N 次查询 | `src/api/admin/admin_transcoding.rs:32`、`web/src/features/admin/AdminTranscodingPage.tsx:39` | 30 次/分 × (1+N) | 管理端观感端点，用户少 | **中** |
| **H3** | HLS 启动路径重复读转码设置与 session 行 | `src/application/playback/session.rs:1075`、`:1104` | 每次启动 1–3 次 | 启动本身已很重 | 低-中 |
| **S2** | 备份 scraper 循环内重复读同一条 item 元数据 | `src/application/reidentify.rs:2144` | 每条目 1–2 次 | 纯可省的读 | 中 |
| **S4** | 每条目解析一次 scraper 配置、读一次能力尝试表，作业内无跨条目复用 | `src/application/scraper.rs:1551`、`src/application/candidates.rs:2805` | 5 万条目作业 = 各 5 万次 | 键查索引，单次便宜 | 中 |
| **M8** | `select_internal` 重复读调用方已持有的 item 元数据 | `src/application/candidates.rs:2996` | 每次选中 1 次 | 读便宜 | 低-中 |
| **H4** | 会话 tier / temp_dir 更新没有"值未变则不写"保护 | `src/storage/sessions.rs:1339`、`:1601` | 每次启动/回退几次 | 与 M3 同类但频率低 | 低 |
| **H6** | 停止转码时 `transcoding_history` 写 + 保留清理两条独立自动提交 | `src/storage/sessions.rs:1230`、`:1262` | 每次转码结束 | 每次多一次提交 | 低 |
| **H7** | `active_server_transcoding_sessions` 的 `ORDER BY updated_at` 无匹配索引 | `src/storage/sessions.rs:1311` | 管理端轮询 | 表内只有 ACTIVE 行 | 低（不建议做） |
| **M6/S7/H8** | 已经是批量/有界/带保护的写入路径（正面记录） | 见各节 | — | 避免后续重复改造 | — |

一句话总结：**HLS 的问题在"每请求一次写事务"，刮削的问题在"每候选一次写事务"，元数据写入的问题在"每演员/每图片/每部电影一次写事务"。**
三者叠加的形态相同——把"每个逻辑对象一次提交"当成了默认，而 SQLite 是单写者 + `BEGIN IMMEDIATE`，
写提交数而不是写字节数才是这三条路径的主要成本。

## 2. 度量方法（与交接 §5、§8 对齐）

交接清单第 8 节要求先建立每请求 SQL 计数。本节给出把本文件结论变成数字的具体做法，便于后续任务实施时做前后对比。

### 2.1 静态口径（本文件用的就是这个）

对着代码数语句：`SELECT`/`INSERT`/`UPDATE`/`DELETE` 各算 1 条，`BEGIN`/`COMMIT` 单独记为"1 次提交"。
`Database` 的薄封装（`src/storage/repository.rs` 的 `query/query_as/query_scalar`）不改变条数。
调用次数乘上触发频率（分片 4 s、心跳 60 s、进度 10 s、管理端轮询 2 s/5 s，均取自代码常量或前端 `refetchInterval`）得到估算。

### 2.2 动态口径

1. **语句计数**：`Database::query_count()` 已存在（`src/storage/repository.rs:1911`，`reset_query_count` 在 `:1882`），
   但目前只在测试里用（`src/application/reidentify.rs:3540` 一带有先例）。交接第 8 节建议的"每请求计数中间件"落地前，
   可以先用 `cargo test` 里已有的 `query_count` 断言给单个用例定基线（例如"一次分片请求的语句数与写提交数"）。
2. **写提交计数**：临时把 `begin_metadata_write_transaction` / `begin_playback_admission_transaction`
   （`repository.rs:1479`、`:1492`）里加一个 `AtomicUsize`，只在本地分支使用，不提交。
3. **查询计划**：在数据副本上 `ANALYZE` 后跑 `EXPLAIN QUERY PLAN`，判断是否走索引、是否 `USE TEMP B-TREE FOR ORDER BY`。
   第 6 节的缺索引结论都可以用这一条验证。
4. **写放大**：`PRAGMA page_count` / `dbstat` 差值，以及 `catalog_outbox`/`search_outbox` 的行增量。
   `scripts/audit-media-items-indexes.py` 是已有的同类证据脚本模板。
5. **NAS A/B**：本机只能做相对比较。协议同交接 §5 W1——同一份库副本、同一素材、`LUX_SQLITE_SYNCHRONOUS=full|normal` 各跑一遍，
   记录 CPU、墙钟、`strace`/`fsync` 次数（或 `powermetrics` 需要 root）。

### 2.3 复现清单（建议后续任务直接拿来当验收用例）

| 场景 | 观测点 | 期望（改造后） |
|---|---|---|
| 单路 HLS 播放 60 s | 分片/清单请求数、`query_count`、写提交数 | 写提交数从 ~30 降到 ≤2（心跳周期） |
| 手动"搜索候选"20 条 | `metadata_capability_attempts` 写入事务数 | 1（循环结束批量写） |
| 单部电影"回填缺失" | 写提交数 | 从 ~50–200 降到 ≤5 |
| 未变更电影复扫 ×100 | `collection_item_references` 的 DELETE/INSERT 次数、`catalog_outbox` 行增量 | 0 / 0 |

## 3. HLS 转码

### 3.1 请求分布（已读代码确认）

HLS 本身（`src/application/playback/hls.rs`）不碰数据库：它只管理 FFmpeg 进程、临时目录和内存快照。
数据库行为全部发生在**资源服务与鉴权**这一段。两条入口：

- Web：`GET/HEAD /api/v1/playback/sessions/{id}/hls/{*asset}` → `lux_web_playback_hls`（`src/api/playback.rs:4303`）
- Emby：`/emby/Videos/{id}/master.m3u8` 与 `.../hls1/{playlist}/{segment}` → `serve_emby_transcoding_asset`（`src/api/playback.rs:3369`）

两者对**每个**分片与清单请求都执行下面这条链路（顺序即代码顺序）：

| # | 语句 | 位置 |
|---|---|---|
| 1 | `SELECT value FROM server_settings WHERE key='trusted_proxy_cidrs'`（仅当请求带 `X-Forwarded-For`，反向代理部署下恒成立） | `src/api/legacy.rs:1802` → `src/application/settings.rs:669` → `src/storage/users.rs:3265` |
| 2 | `SELECT … FROM web_playback_sessions WHERE id = ?` | `src/application/playback/session.rs:900` → `src/storage/sessions.rs:1392` |
| 3 | `SELECT user_id, authorization_device_id, authorization_client_name FROM web_playback_sessions WHERE id = ? AND state='ACTIVE'` | `session.rs:941` → `sessions.rs:1098` |
| 4 | `SELECT … EXISTS(SELECT 1 FROM user_access_blacklist_rules …)` | `session.rs:976` → `src/storage/users.rs:2075` |
| 5 | `SELECT match_value FROM user_access_blacklist_rules WHERE rule_type='IP_CIDR' …` | `session.rs:989` → `users.rs:2135` |
| 6 | `SELECT can_play, max_simultaneous_playbacks, … FROM users WHERE id = ?` | `session.rs:917` → `users.rs:839` |
| 7 | 准入事务：`BEGIN IMMEDIATE` | `session.rs:341` → `sessions.rs:281` |
| 8 | … `SELECT can_play, max_simultaneous_playbacks FROM users WHERE id = ?` | `sessions.rs:287` |
| 9 | … `UPDATE playback_admissions SET state='RELEASED' WHERE user_id=? AND state='ACTIVE' AND last_seen_at <= ?` | `sessions.rs:304` |
| 10 | … `SELECT COUNT(*) … UNION …`（admissions ⋈ playback_sessions） | `sessions.rs:313` |
| 11 | … `INSERT INTO playback_admissions … ON CONFLICT DO UPDATE` | `sessions.rs:356` |
| 12 | … `SELECT COUNT(*) … UNION …`（与 #10 相同形状，再算一次） | `sessions.rs:378` |
| 13 | `COMMIT` | `sessions.rs:395` |

即**每个分片/清单 GET = 6 次读（无代理时 5 次）+ 1 次写事务（事务内 5 条语句）**，其中 #2/#3 读同一行、#6/#8 读同一行。
两处细节会让条数再变：会话没有 `authorization_device_id`/`authorization_client_name` 时会多一次
`active_user_access_device_client_blacklist_rules`（`session.rs:961-972` → `users.rs:2106`）；
`user_access_blacklist_matches` 在 device/client 都为空时直接返回 `false` 不查库（`users.rs:2079-2081`）。
Web 端在创建会话时就写好 device/client（`sessions.rs:1074-1096`），所以稳态是上表的 6 条。
清单分支（`src/api/playback.rs:3473`、`:4354`）与分片分支（`:3519`、`:4397`）都会调用 `admit_web_playback_request`；
只有 `HEAD` 会跳过（`if method == Method::GET`）。

### H1 每个分片请求一次写事务（已读代码确认）

- **位置**：`src/api/playback.rs:4397-4401`（Web 分片）、`:3519-3524`（Emby 分片）、`:4354-4358`/`:3473-3479`（清单）；
  `src/application/playback/session.rs:341-350` → `src/storage/sessions.rs:275-401`。
- **现象**：`admit_web_playback_request` 直接调用 `admit_playback_session`，后者无条件走
  `reserve_playback_admission`（完整准入：陈旧名额回收 + 两次数数 + UPSERT），而不是已存在的
  `renew_existing_playback_admission`（`sessions.rs:403-428`，单条 `UPDATE … SET last_seen_at = max(...)`）。
  两个计数查询形状完全相同（`sessions.rs:313` 与 `:378`），事务内算了两遍。
- **触发频率**：分片 4 s 一个（`hls.rs:36` 的 `HLS_SEGMENT_DURATION_TICKS`）。1 小时素材 ≈ 900 个分片；
  hls.js（`web/package.json` 的 `hls.js@1.7.1`，`web/src/features/player/hls-playback-engine.ts`）与 Emby 客户端
  按标准行为在每个分片边界重载 media playlist（**推测**，未抓包）。按 1 分片 + 1 清单计：
  **单路 ≈ 30 次写提交/分，1 小时电影 ≈ 1800 次提交**；3 路并发 ≈ 5400 次/小时。
- **影响**：`synchronous` 默认 `Full`（`repository.rs:471/477/485` 的兜底；可用 `LUX_SQLITE_SYNCHRONOUS` 覆盖，见交接 W1），
  每次提交有 WAL fsync；且这些写和扫描/元数据写共用同一个 8 连接池与同一个 `BEGIN IMMEDIATE` 写者位。
  机械盘上这是"播放时后台写入持续存在"的主要来源（**推测**，需 NAS 实测）。
- **建议修法**：分片/清单路径改成"先续租，失败才准入"：

  ```rust
  // 稳态只有一个 UPDATE；返回 false（名额被回收/过期）时才走完整 reserve
  if !service.renew_playback_session_admission(&session.user_id, &session.play_session_id).await? {
      service.admit_playback_session(&session.user_id, &session.play_session_id).await?;
  }
  ```

  `renew_existing_playback_admission` 已经存在且语义合适（`WHERE state='ACTIVE' AND last_seen_at > stale_before`，
  返回 `rows_affected()==1`）。这样稳态每分片是 1 条 `UPDATE`（无显式事务），并发上限的完整判定退到
  心跳（60 s）与会话创建时。顺带删掉 `reserve_playback_admission` 里重复的第二次计数（`sessions.rs:378`），
  无论走不走 H1，这条都该删。
- **风险**：**会改变可观察行为**。并发上限不再每 4 s 重新判定，最长滞后一个心跳周期（60 s）；名额被回收后
  在 `renew` 失败前会继续返回分片。按项目规则需要项目所有者确认。另一种更保守的方案是保留 `reserve` 但按
  `last_seen_at` 节流（≥30 s 才重算一次），滞后更短但仍有窗口。
- **是否值得做**：**高**。收益最大、改动局部（4 个调用点 + 一个分支），且 `renew` 已经是现成函数。

### H2 每请求读 6 次，含两处重复读与一次设置行读取（已读代码确认）

- **位置**：`src/application/playback/session.rs:900`（session 行）与 `:941-945`（同一 session 行的另一列子集）；
  `:917-921`（`users` 策略）与 `sessions.rs:287`（同一 `users` 行，事务内再读一次）；
  `src/api/legacy.rs:1797-1804` → `src/application/settings.rs:669-676`（每次请求读 `server_settings` 并 `serde_json` 解析 + CIDR 规范化）。
- **现象**：
  1. `authorize_resource_with_ip` 先 `find_web_playback_session` 拿整行（21 列），随后
     `enforce_playback_access_rules` 又用 `find_web_playback_access_context` 读**同一行**的 3 列。
     `src/application/access.rs` 的 `playback_access_cache`（`:456` 的 `PlaybackAccessKey`）只服务
     `can_view_item_cached`（进度上报路径），**不覆盖**HLS 资源鉴权这条链路。
  2. `server_settings` 的 `trusted_proxy_cidrs` 每次请求都读库 + 重新解析，反代部署下 100% 命中这条。
  3. `users` 策略行在一次请求里读两次（鉴权 + 准入事务）。
- **触发频率**：与 H1 相同（每分片/清单）。单路 1 小时播放 ≈ 1800 请求 × 6 读 ≈ 10 800 条读语句。
- **影响**：读本身是主键/唯一键查找，单条很便宜；但 sqlx 每条语句要走 worker 线程往返（交接 §6 提到
  "每次查询经过 sqlx worker 线程往返"是上下文切换的主因），1 万条读语句 ≈ 1 万次线程往返，在低功耗 NAS 上不是零成本。
- **建议修法**：
  1. 把 `authorization_device_id`/`authorization_client_name` 并入 `find_web_playback_session` 的投影（一次读替代两次），
     `enforce_playback_access_rules` 改成接受 `&StoredWebPlaybackSession`。
  2. `trusted_proxy_cidrs` 加进程内缓存 + 写侧失效（`set_trusted_proxy_cidrs_json`（`users.rs:3275`）已经在写侧汇聚，
     可以复用 `AccessChangeEpochs` 那套 epoch 机制，`src/storage/access_changes.rs`）。
  3. 准入事务里的策略读改为复用第 6 步的结果（把 `UserPlaybackPolicy` 传进 `reserve_playback_admission`）。
- **风险**：缓存失效必须即时（改可信代理、改播放策略）；第 1、3 条是纯重构无行为变化，第 2 条需要显式失效点。
- **是否值得做**：**高**（与 H1 同批做，单请求从 ~11 条语句降到 ~3 条）。第 1、3 条可以先做，零行为变化。

### H3 HLS 启动路径重复读设置与 session 行（已读代码确认）

- **位置**：`src/application/playback/session.rs:1075-1079`（`transcoding_settings` → `src/application/transcoding.rs:299-317`）、
  `:1174`（每次启动再读一次）、`:400`（`create` 里第三次）、`restart_with_software` `:1146`；
  `mark_session_tier` `:1104-1127` 先 `find_web_playback_session` 再 `UPDATE`。
- **现象**：一次 HLS 启动读 2–3 次同一个 `server_settings` JSON 行 + 1–2 次 session 行。
- **触发频率与影响**：每次会话启动 1 次；启动本身要 probe 硬件编码器（`hls.rs` 的 15 s 预算），这点读可忽略。
- **建议修法**：`start_hls_with_runtime` 把已读到的 `TranscodingSettings` 传下去；`mark_session_tier` 接受已知的 `user_id`。
- **风险**：无（纯传参重构）。
- **是否值得做**：**低-中**。顺手做，不单独排任务。

### H4 会话生命周期里的无条件 UPDATE（已读代码确认）

- **位置**：`src/storage/sessions.rs:1339-1363`（`update_web_playback_session_tier`）、`:1601-1618`（`set_web_playback_temp_dir`）。
- **现象**：两条都无条件写 `updated_at`，没有第二/三轮 W3 那种 `IS DISTINCT FROM` 保护。硬件编码失败回退软件
  （`session.rs:1138-1163`）时 tier 与 temp_dir 都会被重写一次，即使值没变；`set_web_playback_temp_dir`
  在 Emby 分片重启用 `start_for_initial_asset` 时可能重复调用（**推测**：取决于重启次数，未逐一追）。
- **触发频率与影响**：每次会话几次，量级小；但这两行是 `web_playback_sessions`，本身没有触发器，写放大只到表自身。
- **建议修法**：加 `WHERE id=? AND (tier IS NOT ? OR plan IS NOT ?)` 之类的保护，或者干脆只在实际变化时调用。
- **风险**：`updated_at` 不再推进；管理端按 `updated_at DESC` 排序（H7）的语义会变，需要确认无人依赖。
- **是否值得做**：**低**。

### H5 管理端转码页 N+1 轮询（已读代码确认）

- **位置**：`src/api/admin/admin_transcoding.rs:23-45`（1 次 `active_server_transcoding_sessions(24)` + 每会话 1 次
  `service.stats(...)`）；`stats` 又读一次 session 行（`src/application/playback/session.rs:1366-1396`）；
  前端 `refetchInterval: 2_000`（`web/src/features/admin/AdminTranscodingPage.tsx:39`）。
- **现象**：一次轮询 = `1 + N` 条查询（N = 活跃 HLS 会话数）。另有 `session_bytes`（`hls.rs:1550-1558` →
  `session_directory_bytes`）**每次轮询遍历一次会话临时目录**求和字节数；`hls_within_quota` 用的
  `cached_session_bytes`（`hls.rs:1560`）有 5 s TTL，但 `stats` 走的是不走缓存的 `session_bytes`。
- **触发频率与影响**：页面打开时 30 次/分 × (1+N) ≈ N=3 时 120 查询/分，外加 30 次目录遍历（每会话）。管理端场景，用户少，
  但它是"边播放边看转码页"时的叠加成本。
- **建议修法**：列表查询已经带出 `tier/state/codecs/container` 等列（`sessions.rs:1319-1327`），
  `stats` 不必再读 session 行——改成直接吃列表行 + 内存快照；`directory_bytes` 复用 `cached_session_bytes`
  或在展开行时再取。
- **风险**：统计页显示的 `directoryBytes` 最多滞后 5 s。
- **是否值得做**：**中**（成本低，收益主要在管理端）。

### H6 `transcoding_history` 两次自动提交 + 管理端 COUNT（已读代码确认）

- **位置**：`src/storage/sessions.rs:1230-1271`（先 `INSERT`，随后另一条 `DELETE … WHERE finished_at < ?` 做保留清理，
  两条各自自动提交；保留期 `:1744` 为 30 天）、`:1274-1312`（`transcoding_history_page` 一次 `COUNT(*)` +
  双 `LEFT JOIN` + `ORDER BY h.finished_at DESC`，索引 `migrations/0224_transcoding_history.sql` 的
  `idx_transcoding_history_finished` 可覆盖排序）。
- **现象**：两次写分散在两个自动提交里；翻页查询的 COUNT 与双 LEFT JOIN 都是每次现场算。
- **触发频率与影响**：每次转码结束多一次提交；COUNT 在 30 天保留期内可接受。
- **建议修法**：把 DELETE 并进 INSERT 的同一事务（或移出到 `database_cleanup` 的后台清理）；COUNT 保持现状。
- **风险**：无。
- **是否值得做**：**低**。

### H7 `ORDER BY updated_at` 无匹配索引（已读代码确认，建议不做）

- **位置**：`src/storage/sessions.rs:1311-1337`（`WHERE state='ACTIVE' AND plan='SERVER_HLS' ORDER BY w.updated_at DESC`），
  现有索引是 `idx_web_playback_sessions_expiry(state, expires_at)`（`migrations/` 建表文件）。
- **现象**：过滤条件里没有 `updated_at` 之外的可用索引列组合；计划器会先按 `(state, expires_at)` 取 ACTIVE 行再排序。
- **触发频率与影响**：管理端转码页轮询 30 次/分（每次 1 条该查询）；但 `web_playback_sessions` 里 ACTIVE 行数量
  等于同时播放数（个位数），排序成本可忽略。
- **建议**：**不加索引**。加了反而增加这张高频更新表的写成本（每 60 s 心跳都 UPDATE）。
- **风险**：无（不做）。
- **是否值得做**：**低（明确不做）**。

### H8 事件路径（已读代码确认，属交接 §1 范围）

- **位置**：`src/application/playback/session.rs:1502-1545`（`claim_event`）→ `src/storage/sessions.rs:1667-1720`
  （`accept_web_playback_event`：一个事务，先 `INSERT … ON CONFLICT DO NOTHING` 写 `web_playback_events`，再 `UPDATE web_playback_sessions`）；
  `claim_event` 落库后又 `find_web_playback_session` 读一次同一行（`:1530-1533`）。
- **现象**：每个事件一个事务（INSERT 事件行 + UPDATE 会话行），落库后同一个请求里又读一次会话行。
- **触发频率与影响**：进度 10 s 一次、心跳 60 s 一次（`web/src/features/player/usePlaybackReporting.ts`）。
  另注意 `lux_web_playback_event` 的重复事件分支仍会调用 `renew_playback_session_admission`
  （`src/api/playback.rs:4630-4636`），即重试也写一次。
- **建议**：这是交接 §1 已经覆盖的合并写入范畴，本审计只标注"HLS 会话同样付这份成本"。
- **风险**：与 §1 相同（进度合并会影响"继续观看"的新鲜度）。
- **是否值得做**：**低-中**（与 §1 一起做，不单独排）。

### 3.2 已确认做对、不需要改的部分

- `hls_segment_container`（`session.rs:1348`）、`hls_within_quota`（`:1355`）、`has_started`（`hls.rs:1505`）
  都是内存读；`activate_emby_hls_session`（`session.rs:1285`）在会话已启动时只做一次内存判断就返回
  （`hls.rs:1505-1515`），不会每分片读库——`find_web_playback_session`/`find_active_web_playback_sessions_for_source`
  只在"尚未启动"的首次请求发生。
- `find_active_web_playback_sessions_for_source`（`sessions.rs:1445-1478`）的 `WHERE user_id = ?` 走
  `UNIQUE (user_id, play_session_id)` 前缀，`LIKE 'lux-emby:%'` 只在本人会话里过滤，不是全表扫描。
- `playback_admissions` 有 `PRIMARY KEY (user_id, play_session_id)` 与 `idx_playback_admissions_active(user_id, state, last_seen_at)`
  （`migrations/0186_user_playback_admissions.sql:16`），#9–#12 都是索引范围操作。
- `web_playback_sessions` 清理（`src/storage/database_cleanup.rs:1183-1197`）是有界批量 + 索引范围。
- 管理端转码历史保留 30 天，COUNT 不会随时间无限退化。

## 4. 刮削（TMDb）

### 4.1 单条目流程与语句清单（已读代码确认）

作业入口 `MetadataReidentifyService::run`（`src/application/reidentify.rs:1359`）按并发 worker 领取条目
（`claim_next_metadata_reidentify_items`，`:1480-1483`），每个条目走 `process_item`（`:1660`）：

| 阶段 | DB 操作 | 位置 |
|---|---|---|
| 读条目 | `find_media_item_metadata` × 1 | `reidentify.rs:1669` |
| 解析 scraper | `list_item_scraper_configurations_by_ids([item])` × 1，随后按 scraper 逐个 `client_for_scraper`（内存缓存，按 revision） | `src/application/scraper.rs:1551`、`:1449` |
| 搜索候选 | 1–20 个候选 ×（详情/bundle + 图片 + credits + external ids + trailers 的 TMDb 调用）；**每个候选写 1–2 次能力表** | `src/application/candidates.rs:831`、`:1105`、`:1116` |
| 落候选 | 1 次批量 upsert（分块 100） | `candidates.rs:1726` → `src/storage/metadata.rs:1032` |
| 选中候选 | 备份循环内重复读条目 + 读候选 + 按需读 NFO 投影 | `reidentify.rs:2144`、`candidates.rs:2996`/`:3001`/`:3058` |
| 应用选择 | 图片 / 演员 / NFO / 元数据四组写入（见第 5 节） | `candidates.rs:3093`/`:3132`/`:3138`/`:3202` |
| 记录结果 | `finish_metadata_reidentify_item` × 1 | `reidentify.rs:1767-1776` |

### S1 候选循环内每个候选一个写事务（已读代码确认）

- **位置**：`src/application/candidates.rs:831`（`for (result_index, (result, precomputed_score)) in results…`）
  循环体内的 `:1105-1114`（`record_metadata_capability_results`）与 `:1116-1126`（`record_metadata_capability_failures`）；
  实现 `src/storage/metadata.rs:1252-1316`（`:1273` 取全局元数据写锁、`:1274` `BEGIN IMMEDIATE`）与 `:1318-1408`。
- **现象**：`search_and_store_with_mode` 对搜索结果逐条抓详情（最多 20 条，手动搜索 `plan = full`），
  每抓到一条就立刻写一次能力表（成功一次 + 有瞬时失败再一次）。两次调用各自开事务、各自取写锁。
- **触发频率**：用户在条目页点"搜索候选"（手动路径，最多 20 条结果）；自动匹配通常只 1–2 条候选。
  5 万条目全量回填作业里，每个条目 1–2 个候选 → 5 万 × 1–2 次提交。
- **影响**：每次提交都在 SQLite 单写者位上，和扫描/播放写路径互相排队；`record_metadata_capability_results`
  本身已经把同一候选的多个能力合成一条多行 upsert（这点写得很对），问题只在于**调用次数**。
- **建议修法**：循环里只 `push` 到 `Vec<MetadataCapabilityResult>`（按 `(provider, provider_id)` 分组），
  循环结束后对每个 `(provider, provider_id)` 调一次；`capability_failures` 同样合并。
  函数签名已在 `src/storage/metadata.rs:1252`，只需在 application 层改聚合位置。
- **风险**：失败语义——现在是"抓到一条就记一条"，改成循环后聚合会让"后续候选让整个搜索报错"时丢掉已抓到的结果。
  缓解：聚合写入放在 `for` 之后、错误返回之前，并保留 `?` 之前的部分写入（把写事务放在错误处理之外）。
- **是否值得做**：**高**。这是刮削路径最常见的用户操作。

### S2 备份 scraper 循环内重复读条目元数据（已读代码确认）

- **位置**：`src/application/reidentify.rs:2144-2150`（`for scraper in scrapers.iter().filter(backup/both)` 内
  `find_media_item_metadata(item_id)`）。
- **现象**：第一次读到的 `current` 只用于 `fallback_request_plan_for_current`；下一个备选 scraper 再读一次同一行。
  循环上界是 scraper 数（通常 1–3）。
- **触发频率与影响**：每个条目 1–2 次多余读（每条目已有一次 baseline 读，`reidentify.rs:1669`）；
  单次是主键键查，影响很小，属于"纯可省"。
- **建议修法**：用 `let mut current = item.clone()` 在循环外持有，只在前一次 `refresh_item` 真的写入后重读
  （`refresh_item` 会调用 `select_*` 改写条目）。
- **风险**：如果复用陈旧快照，`fallback_request_plan_for_current` 会误判"哪些能力还缺"。
  正确做法是"写入后失效"，不是"永远复用"。
- **是否值得做**：**中**。

### S3 调用方已持有 current 时仍会再读一次（已读代码确认）

- **位置**：`src/application/candidates.rs:671-683`（`let fetched_current = if current.is_none() { find_media_item_metadata… }`）。
- **现象**：`refresh_item` 在 `Reidentify`/`FillMissing` 分支把 `current` 传了进去，但 `search_and_store`
  （`candidates.rs:548` 的公开入口，供手动搜索使用）不传；每次手动搜索多一次条目读。
- **触发频率与影响**：每次手动"搜索候选"多 1 次条目读（宽投影，含 NFO 缓存列）；批量回填路径已传
  `current`，不会触发。
- **建议修法**：手动搜索入口（`reidentify.rs:1126-1148`）先读一次条目并传下去，避免"读 kind + 读 metadata"两遍。
- **风险**：无。
- **是否值得做**：**低-中**。

### S4 作业内没有跨条目复用配置与尝试历史（已读代码确认）

- **位置**：`src/application/scraper.rs:1551-1557`（每条目一次 `list_item_scraper_configurations_by_ids([item_id])`）；
  `src/application/candidates.rs:2805`（每条目一次 `list_metadata_attempts(item_id)`）。
- **现象**：同一媒体库的 5 万条目会重复查 5 万次"这个条目用什么 scraper"。代码里已有批量接口
  `has_selected_scrapers_for_items`（`scraper.rs:1562`）与 `list_metadata_attempts_by_item_ids`
  （`candidates.rs:2627` 的调用点），但主流程走的是单条目版本。
- **触发频率与影响**：每条目各 1 次（5 万条目作业 = 各 5 万次）。都是索引键查
  （`idx_*_scraper*`、`idx_metadata_capability_attempts_item`），单次成本低；
  5 万条目 ≈ 10 万条读语句，占作业总语句数的比例不大（大头是 TMDb 往返）。
- **建议修法**：`claim_next_metadata_reidentify_items` 已经一次领取多个条目（`reidentify.rs:1480-1483` 一次取
  `worker_permits.len()` 个），可以在领取后批量预取这两张表的相关行，随 `process_item` 传入。
- **风险**：低（需要保证缓存能反映本条目在作业期间的写入）。
- **是否值得做**：**中**（大库回填时省 10 万次往返，非最优先）。

### S5 过期 PENDING 候选清理缺索引（已读代码确认）

- **位置**：`src/storage/database_cleanup.rs:929-937`（完整论证见第 6 节 I1）。
- **现象**：清理语句按 `ORDER BY expires_at` 取 PENDING 候选，但 `metadata_candidates` 没有 `expires_at` 索引
  （`migrations/0013_metadata_candidates.sql:14-15`）。
- **触发频率与影响**：数据库清理任务每次一批；候选积压时排序集 = 全部 PENDING 行。
- **建议修法**：与 I1 一起加 `(status, expires_at, id)` 部分索引。
- **风险**：候选表多一个索引，写成本略增。
- **是否值得做**：**中**（与 I1 一起，走迁移）。

### S6 `jobs` SSE 事件的管理端放大（已读代码确认 + 频率推测）

- **位置**：`web/src/features/admin/useAdminEvents.ts:18-33`（scope `jobs` 一次失效 10 个查询键，含 `["admin","metadata-jobs"]`、
  `["admin","jobs"]`、`["admin","task-activity"]`、`["admin","scheduled-tasks"]`、`["admin","dashboard"]`、`["admin","logs"]`）；
  后端发布门限 `METADATA_PROGRESS_EVENT_INTERVAL = 1 s`（`src/application/reidentify.rs:54`，`publish_job_progress` 在 `:1969`）。
- **现象**：元数据作业运行时，只要管理端任务页开着，服务端每秒收到一轮 10 个端点请求。其中"元数据任务列表"带
  `pending_counts` 聚合：`job_items ⋈ metadata_candidates` 按 `job_id` `COUNT(DISTINCT item_id)`
  （`src/storage/jobs/metadata_reidentify/lifecycle.rs:186-193`）；`find_metadata_reidentify_job` 也带同样的聚合（`:143-151`）。
- **触发频率与影响**：任务运行时 ≤1 次/秒 × 10 个端点（页面打开时）。一个 5 万条目的作业 = 5 万行 `job_items`
  × 每行按 `(item_id, status='PENDING')` 探一次 `idx_metadata_candidates_item`，每秒跑一次
  （**推测**：取决于页面是否订阅 `jobs` 事件、作业是否活跃；两个条件在"一边跑回填一边看任务页"时同时成立）。
- **建议修法**：把 `pending_count` 改成由写侧维护的计数器列（`metadata_reidentify_jobs.pending_count` 已经在结构体里存在，
  `reidentify.rs:2705`，只是由聚合算出来），或对列表接口改成"只在请求带 `?includePending=1` 时算"。
- **风险**：计数列需要与候选状态变更保持同步（`select/reject/expire` 三处）。
- **是否值得做**：**中**（管理端，但与交接 §2 的 SSE 放大是同一类问题，值得一并处理）。

### S7 已确认正常、不需要改的部分

以下每条都给出位置与判断；结论一律是"不改"，价值为 **无**。

- 候选列表读路径索引齐全：`list_pending_metadata_candidates`（`src/storage/metadata.rs:1410-1435`）走
  `idx_metadata_candidates_status(status, created_at, id)`；按条目列表（`:1465-1512`）走
  `idx_metadata_candidates_item(item_id, status, created_at, id)`；`ORDER BY created_at, id` 与索引顺序一致，无临时 B 树。
  `candidate_json LIKE '%…%'` 的搜索只在该条目的候选集合内扫（`WHERE mc.item_id = ?`），不是全表。
- 候选写入已批量：`insert_metadata_candidates`（`src/storage/metadata.rs:1032-1107`）分块 100、单事务、取写锁一次。
- 能力表写入已批量：`record_metadata_capability_results`（`:1252-1316`）单事务多行 upsert（问题只在调用次数，见 S1）。
- 演员名映射已批量：`find_canonical_people_by_identities`（`src/application/people/relations.rs:504-513`）。
- `list_best_pending_metadata_candidates_for_item` 等读接口都带 `LIMIT`。
- TMDb 调用放大（`search_generic` 的标题候选 × 年份串行重试，`candidates.rs:1844-1881`）**不产生 SQL**，
  只影响作业时长；本审计不把它算作 DB 问题（**推测**：它会让 S1 写事务的分布更分散，而非更集中）。

**是否值得做**：**无**（以上均为已达标项或非 DB 问题，风险为"无"）。

## 5. 元数据写入

### 5.1 单条目写入的事务清单（已读代码确认）

"应用选中候选"是元数据写入的主路径：`select_internal`（`src/application/candidates.rs:2989-3239`）。
它不是一次事务，而是**四组独立事务**：

| # | 写入 | 事务边界 | 位置 |
|---|---|---|---|
| 1 | 图片（每张图 claim + 图片行 + finish） | 每张图 2–3 次 `BEGIN IMMEDIATE` + 全局元数据写锁 | `candidates.rs:3093-3105` → `src/application/images.rs:528`/`:549` → `src/storage/catalog.rs:8824` |
| 2 | 演员关系（`replace_person_credits_with_relation_checksum`，1 次）+ 每个演员 1–2 次 person 解析事务 | 1 + O(演员数) | `candidates.rs:3132-3135`；`src/application/people/relations.rs:516-637` |
| 3 | NFO 文件 + `sync_media_item_nfo_state` | 1 次（自带事务与写锁） | `candidates.rs:3138-3141` → `src/application/nfo.rs:3101-3133`/`:3229` → `catalog.rs:8764` |
| 4 | 条目元数据 + 候选状态 + NFO 缓存 + 集合引用 | 1 次事务（内部多条语句） | `candidates.rs:3202-3218` → `src/storage/metadata.rs:1727-1849` |

### 5.2 三条路径共享的约束：单写者 + 全局元数据写锁

- **已读代码确认**：本文件提到的多数元数据写都先拿进程内的全局锁，再开 `BEGIN IMMEDIATE`
  （`repository.rs:1439-1460` 的 `acquire_metadata_write_lock`，SQLite 下是单实例互斥；
  `repository.rs:1479-1488` 的 `begin_metadata_write_transaction` 在 SQLite 上用 `BEGIN IMMEDIATE`）。
  也有只开 `BEGIN IMMEDIATE`、不取进程内锁的（例如 M2 的 `resolve_or_create_canonical_person`）。
  HLS 的准入写同样走 `BEGIN IMMEDIATE`（`sessions.rs:281`、`:395`）。无论取不取那把进程内锁，
  最终都排在同一张 SQLite 写者位上。
- **含义**：M2/M5/M4 的"N 次提交"不是"N 次便宜的写"，而是"N 次拿到同一把锁再交还"。
  一个 100 演员的条目在写入期间会反复占住这把锁，扫描的写事务、HLS 的准入事务都要排队。
  这也是为什么本文件把"提交次数"而不是"写入字节数"当作主要指标。
- **对建议的影响**：批次 1 的几条（M2 合并事务、M4 跳过无变化、M5 合并 finish）都在减少拿锁次数；
  如果只做 M3（加 UPDATE 条件）而不做这些，锁竞争不会明显改善。

### M1 应用选择没有总事务，四组写入各自提交（已读代码确认）

- **位置**：上表 #1–#4，集中在 `src/application/candidates.rs:3093-3223`。
- **现象**：中途任何一步失败（图片下载失败、演员关系文件锁超时、NFO 写失败），前面已提交的组不会回退：
  典型状态是"图片已写、`metadata_candidates` 仍是 PENDING"。第 4 组内部有自己的事务与 rollback
  （`src/storage/metadata.rs:1823-1826` 在候选已被并发选中时回滚），但它管不到前三组。
- **触发频率**：每个条目每次选中/回填刷新 1 次。
- **影响**：主要是**正确性**（部分状态），性能上是"重试时把前三组再做一遍"。因为这些写基本幂等
  （图片有 attempt 表去重、演员关系有 checksum、NFO 有内容指纹），重复执行的代价是成倍提交而非错误数据。
- **建议修法**：
  1. 把"外部副作用"（图片下载、NFO 文件写）与"数据库状态"分开：先完成全部外部下载到临时位置，
     再在一个事务里写图片行 + 演员 + 条目元数据 + 候选状态。这是结构性改造，风险较高。
  2. 成本低的做法：让 #2 的演员解析并入 #4 的事务（见 M2），把四组降到"三组"，并给 #1 的 finish 与图片行合并（见 M5）。
  3. 若接受现状，至少在失败路径显式记录"部分应用"，避免调用方把 PENDING 当成"没写过"。
- **风险**：方案 1 会改写入顺序与失败语义，需要所有者确认"图片/NFO 外部副作用不入库事务"的边界是否可接受。
- **是否值得做**：**中**（性能收益来自 M2/M5；本条的独立价值是正确性）。

### M2 演员落库：每个演员一个写事务（已读代码确认）

- **位置**：`src/application/people/relations.rs:516-637` 的 `for (actor, identities, bridge_candidates) in prepared_actors`
  循环体，`:626-634` 调 `resolve_person_key`；实现 `relations.rs:43-180`，其中 `:149-160` 调
  `resolve_or_create_canonical_person`（`src/storage/people.rs:612-640`，`:622` 就是 `begin_metadata_write_transaction`）；
  多身份演员还会再走 `attach_canonical_person_identities`（`people.rs:1712-1722`，又一个事务）。
  上限 `src/application/people/service.rs:77`：`MAX_ACTORS = 100`。
- **现象**：即使演员已经存在，`resolve_or_create_canonical_person` 也是"开事务 → `SELECT p.id FROM people JOIN person_identities …`
  （走 `person_identities` 的主键 `(provider, provider_id)`，`migrations/0073_canonical_people.sql:28`）→ 提交"。
  所以一条 100 演员的剧集 = **100 次 `BEGIN IMMEDIATE` + 100 次 `COMMIT`**（新演员再加 INSERT 与 identity 写入）。
  这些事务全部串行（SQLite 单写者位；该函数只开 `BEGIN IMMEDIATE`，不额外取进程内的元数据写锁），
  并且整段还持着 `relations.rs:424-427` 的**文件锁**。
- **触发频率**：每次元数据选中；每次带 cast 的本地 NFO 扫描（DeferredNfoActorCredits 路径同样走逐个解析）。
  一部 2 小时电影平均 30–60 演员，一套剧集每集 10–30。全库扫描 5 000 部电影按 40 演员算 ≈ 20 万次提交。
- **影响**：这是三条路径里最大的提交放大。稳态（演员都存在）下每次提交没有页面变更，WAL 不需要 fsync，
  成本主要是"拿锁 + 2 次往返"，仍然可观；新演员时每次插入 `people` + `person_identities` 并 fsync，机械盘上
  30–60 ms/演员是可能量级（**推测**，需 NAS 实测）。
- **建议修法**：
  1. 批量预解析：先一次 `find_canonical_people_by_identities` 拿到全部已存在的 `person_key`（这步已经在做，`relations.rs:504`），
     把"还需要新建"的演员收集起来，用一次多行 `INSERT INTO people …` + 一次多行 `INSERT INTO person_identities …`
     （`person_id_sequence` 用一次 `INSERT … RETURNING` 取 N 个号）完成。
  2. 保守做法：新增 `resolve_or_create_canonical_person_in_transaction(&mut tx, …)`，把整个演员循环放进**一个**事务里。
     这样 100 次 begin/commit 变成 1 次，语义完全不变，改动局限在 `src/storage/people.rs` + `relations.rs` 的调用签名。
- **风险**：`person_id_sequence` 与并发创建要保住唯一性（现在靠 `BEGIN IMMEDIATE` 串行 + 重试循环
  `people.rs:642-658`），合并成一个大事务后仍然是单写者，但事务变长，会拉长其他写者的等待；
  需要确认最坏情况（100 演员、新库）的事务时长可接受。
- **是否值得做**：**高**。建议先做方案 2（语义不变、风险低），再评估方案 1。

### M3 元数据选中无条件 `UPDATE media_items`（已读代码确认）

- **位置**：`src/storage/metadata.rs:1742-1785`（`UPDATE media_items SET title=?, sort_title=?, … updated_at=unixepoch() WHERE id=? AND removed_at IS NULL`）。
- **现象**：整行 UPDATE 没有"值未变则不写"条件，属于交接第 5 节 W3 的分类，且是 W3 第二轮**没有覆盖**的那一条
  （第二轮 13 条保护集中在 `sync_media_item_nfo_state`、`restore_media_item`、NFO 复核等路径，
  见交接 §5「已实施（W3）」）。SQLite 的 UPDATE 即使赋的是相同值也会写页并触发触发器；
  `media_items` 的 `media_items_catalog_au` 是**无列清单、无 WHEN** 的整行触发器，每次最多写 5 行 `catalog_outbox`
  （`migrations/0238_catalog_items_incremental_outbox.sql:39`，成本分析见 `docs/INDEX-AUDIT.md` §7）。
- **触发频率**：每次选中候选 / 每次回填刷新写入（回填在 plan 已完整时会提前返回 `Confirmed(0)`，
  `reidentify.rs:1695-1699`，所以"完全没变化"的条目不会走到这里；真的走到这里时通常会有一两列变化）。
- **影响**：一次写入 = 1 次行更新 + 最多 5 行 outbox + 后台读模型重建（`catalog_items` 增量消费者）。
  当值真没变时，这整条链是纯浪费；当只有 `updated_at` 变化时也一样。
- **建议修法**：沿用 W3 的写法，把条件覆盖到语句赋值的每一列（含 `updated_at` 不参与条件），
  例如 `WHERE id = ? AND removed_at IS NULL AND (title IS NOT ? OR sort_title IS NOT ? OR …)`。
  写法与 `sync_media_item_nfo_state`（`catalog.rs:8786-8800` 一带的注释与条件）保持一致。
- **风险**：`updated_at` 不再推进 → Emby 的 `DateLastMediaAdded`/`updatedAt` 语义变化；需确认没有"靠 updated_at
  变化驱动"的读模型刷新逻辑（`media_items_updated_at_ai` 触发器只处理 INSERT 时 `updated_at=0`，不在此路径）。
  另外 `rows_affected()` 在当前调用链没有被当作信号（`select_metadata_candidate_with_cache_and_collection_reference`
  只检查候选状态更新，`src/storage/metadata.rs:1823`），所以加条件本身不破坏语义。
- **是否值得做**：**中-高**。

### M4 未变更的 NFO 每次扫描仍重写 `collection_item_references`（已读代码确认）

- **位置**：`src/application/metadata.rs:3471` 与 `:3532`（两条"跳过"分支里都调 `refresh_cached_movie_collection_reference`）
  → `src/application/metadata.rs:3346-3360` → `src/storage/catalog.rs:7691-7707`（自己开事务 + 写锁）→ `:7828` 无条件
  `DELETE FROM collection_item_references WHERE item_id = ?` → `:7836` 再 `INSERT`。
- **现象**：只有"`reference` 为 `None` **且**带未限定 NFO set id"这一条分支会比较现值并提前返回
  （`catalog.rs:7782-7827`，含 `matches_existing_nfo_identity`）；**`reference` 为 `Some` 时（电影属于某个合集）
  直接 DELETE+INSERT**，不看现值。而这两条调用点正是"NFO 指纹未变、直接跳过解析"的快路径——
  也就是说**没有变化的条目反而每次扫描都写一次**。
- **触发频率**：每个 MOVIE 每次 NFO 复扫 1 次写事务。5 000 部电影的全库复扫 = 5 000 次提交；
  这些提交与扫描自身的写事务抢同一个 `BEGIN IMMEDIATE` 写者位。
- **影响**：每次是 1 DELETE + 1 INSERT + 1 commit。行数很小，但提交次数与扫描条目数线性相关；
  而且 `collection_item_references` 自带两个触发器（`migrations/0171_global_collection_sync_outbox.sql:31` 的
  `collection_item_references_sync_ai` 与 `:65` 的 `collection_item_references_sync_ad`），DELETE+INSERT 一次就
  **把同一个合集的 `collection_sync_outbox.generation` 加 2**，后台全局合集同步消费者因此重新派生该合集
  （消费者在飞行中会合并同一 generation 的重复请求——**推测**，未逐行核对消费者循环）。
  也就是说"没变化的电影"每次复扫都会推动一次合集对账。
- **建议修法**：
  1. 最小改动：在 `replace_global_movie_collection_reference_in_transaction` 里把现值比较扩展到
     `reference.is_some()` 的情况（先 `SELECT` 现有 7 列比较，相同则 `return Ok(())`）。
  2. 更彻底：这条路径已经有批量接口 `commit_local_nfo_state_batch_with_collection_references`
     （`catalog.rs:7718`）与 `GlobalMovieCollectionReferenceUpdate`，把引用更新并入页级 `deferred_metadata_updates`
     批次（`src/application/metadata.rs:2255-2256` 的 flush），一次事务写整页。
- **风险**：低。引用是派生数据，幂等；方案 1 不改变任何可见值。
- **是否值得做**：**高**（改动小、频率高、与扫描写路径直接竞争）。

### M5 每张图片 3 个写事务，串行在元数据写锁上（已读代码确认）

- **位置**：`src/application/images.rs:528-536`（`claim_metadata_image_attempt`）→ `src/storage/catalog.rs:8824-8875`
  （`:8833` 写锁、`:8834` `BEGIN IMMEDIATE`）；`images.rs:549-561`（`finish_metadata_image_attempt`）；
  图片行写在 `images.rs:1070` 的 `download_item_image_impl` 内（`upsert_item_image_at_index`）。
- **现象**：一条目一次选中会为 POSTER/THUMB/FANART/BANNER/LOGO/DISC 等类型各下一张（`candidates.rs:3309-3369` 的 specs
  构造），每张图走"claim（写事务）→ 下载 → 写图片行（写事务）→ finish（写事务）"。
  `write_selected_images` 用 `IMAGE_ITEM_CONCURRENCY` 并发下载（`candidates.rs:3370-3449`），
  但每个任务的 DB 阶段都在**同一个全局元数据写锁**上排队，并发只体现在网络下载段。
- **触发频率**：每条目 3–8 张图 → 约 10–24 次提交（外加变体写入）。
- **影响**：与 M2 同类的提交放大，量级次之。另外写锁持有时间短但次数多，会拉长其他写者（扫描、M3/M4）的等待。
- **建议修法**：
  1. 把 finish 与图片行写入合并成一个事务（少 1 次提交/图）。
  2. `claim` 的意义是"跨进程/跨重启的租约"（表里有 `claimed_until`、`attempt_count`）。
     如果只在单进程内需要去重，可以换成内存租约，把每图 2 次表事务降为 1 次。
  3. 批量 claim：一次事务为整个条目的 N 个候选键 claim（多行 upsert）。
- **风险**：方案 2 会丢掉"重启后不重复下载"的保证（`claimed_until` 是持久租约），需要所有者确认是否可接受；
  方案 1、3 无行为变化。
- **是否值得做**：**中**（方案 1 + 3 先做）。

### M6 NFO 写路径已有 W3 保护（已读代码确认，记录为"已覆盖"）

- **位置**：`src/application/nfo.rs:3101-3133`（`write_item_movie_nfo`：1 次上下文查询 `find_movie_nfo_writeback_context`，
  已是单条查询）、`:3240-3275`（`mirror_item_nfo_if_enabled`：1 次写回策略查询 + 文件写）、
  `:3229-3231` → `src/storage/catalog.rs:8764-8800`（`sync_media_item_nfo_state`：DELETE + UPDATE 带
  "值未变不写"保护和注释说明）。
- **现象**：这条路已经是 W3 的形状（UPDATE 带"值未变不写"条件），**不需要再改**。
- **触发频率与影响**：每次 NFO 写 1 次（每条目每次选中/扫描一次），3–4 条语句 + 1 次提交，成本已接近下界。
- **建议修法**：不改。可选项：`nfo.rs:1287-1320` 的 `read_item` 在缓存命中时仍要 1 次
  `media_item_nfo_validity` 读（`:1292-1296`），这是"缓存校验"的取舍，不算缺陷。
- **风险**：无。
- **是否值得做**：**否（已达标）**。

### M7 本地 NFO 扫描批处理已经批量 + 有界并发（已读代码确认，正面记录）

- **位置**：`src/application/metadata.rs:2115-2140`（批量 `list_current_scan_local_metadata_item_ids`）、
  `:2161-2164`（批量 `list_media_item_metadata_by_ids`）、`:2191-2223`（`run_bounded_tasks_in_order`，并发常量
  `LOCAL_MOVIE_NFO_ENRICH_CONCURRENCY`）、`:2255-2256`（页级 `flush_deferred_local_nfo_metadata_updates`）；
  `src/storage/jobs/local_metadata/batches.rs`（批次入队/领取/完成都是单事务批量）；
  `src/storage/catalog.rs:8661+`（`update_media_item_nfo_metadata_batch`）。
- **现象**：本地元数据扫描这条主干**已经是批量的**（批量读 + 页级 flush + 有界并发）。
- **触发频率与影响**：全库扫描时每页 1 次批量写；M2/M4 是这条主干里仅剩的两个"逐条写"缺口。
- **建议修法**：不改；后续改造不要退化这里（新增的逐条目写应尽量走 `DeferredLocalNfoMetadataUpdates` 的页级 flush）。
- **风险**：无。
- **是否值得做**：**否（已达标）**。

### M8 `select_internal` 重复读调用方已持有的条目（已读代码确认）

- **位置**：`src/application/candidates.rs:2996-3000`（`find_media_item_metadata(item_id)`）；
  调用方 `src/application/reidentify.rs:1670-1725` 已经把 `item` 传给了 `refresh_with_scraper_roles`。
- **现象**：同一个条目在"选中候选"这一步被第二次完整读取（宽投影，含 `nfo_metadata_json`）。
- **触发频率与影响**：每次选中 1 次；单条主键键查但投影很宽（`media_items` 行 + NFO 缓存列），可省。
- **建议修法**：加一个接受 `&StoredMediaMetadata` 的 `select_internal_with_current` 重载
  （模式与 `search_and_store_*_for_current` 一致，`candidates.rs:608-651` 已有先例）。
- **风险**：调用方传入的快照必须在写路径开始前仍然有效（选中过程会改写条目，所以只有入口处可用）。
- **是否值得做**：**低-中**。

### M9 其它已确认的小项（正面记录）

每一行都给出位置、现象/影响与价值；结论一律是"不改"。

| 项 | 位置 | 现象与影响 | 价值 |
|---|---|---|---|
| 填空检查批量 | `src/storage/metadata.rs:37-120`（`prepare_and_claim_item_metadata_completeness_checks`，单事务 + `lock_media_items_for_update` + 批量 upsert） | 已经是批量 | 不改 |
| 条目元数据批量读 | `reidentify.rs:1284-1292`（`list_media_item_metadata_by_ids`） | 已经是批量 | 不改 |
| 演员附件任务 | `relations.rs:798-830`（`LOCAL_NFO_PERSON_ASSET_BATCH_CONCURRENCY` 有界并发） | 并发有界 | 不改 |
| 图片 attempt 重试表 | `migrations/0099_metadata_image_attempts.sql:16`（`idx_metadata_image_attempts_retry(status, next_retry_at)`） | 重试扫描有索引 | 不改 |

**是否值得做**：**无**（以上均为已达标项，风险为"无"）。

## 6. 缺失索引汇总

### I1 `metadata_candidates` 缺 `(status, expires_at)`（已读代码确认）

- **位置**：清理语句 `src/storage/database_cleanup.rs:929-937`
  （`WHERE status='PENDING' AND expires_at IS NOT NULL AND expires_at < ? ORDER BY expires_at, id LIMIT ?`）；
  现有索引只有 `migrations/0013_metadata_candidates.sql:14-15`
  （`(status, created_at, id)`、`(item_id, status, created_at, id)`）与 `0095` 的 pending 唯一索引。
- **现象**：可以用 `(status, created_at, id)` 定位 PENDING 行，但 `ORDER BY expires_at` 无法利用索引顺序 →
  `USE TEMP B-TREE FOR ORDER BY`，排序集 = 全部 PENDING 候选。`expires_at` 上没有索引。
  注释（`database_cleanup.rs:925-927`）写的是"after a one-hour grace"，说明这条是按时间清理的预期路径。
- **触发频率**：数据库清理任务（后台），每次一批。
- **影响**：PENDING 候选积压时（大量条目搜索后未选中）排序成本与积压量成正比（**推测**：候选 24 h 过期，
  积压上限大约是"一天内的搜索量"，全库回填时会很大）。
- **建议修法**：新增部分索引 `CREATE INDEX … ON metadata_candidates(status, expires_at, id) WHERE status = 'PENDING'`
  （SQLite + PG 配对迁移，`scripts/check-migration-parity.py` 会校验）。
- **风险**：多一个索引 = 候选表写成本略增（候选写入已经是批量，影响有限）。
- **是否值得做**：**中**。

### I2 `web_playback_sessions(state, plan, updated_at)`（已读代码确认，建议不做）

- **位置**：`src/storage/sessions.rs:1311-1337`。
- **现象**：`WHERE state='ACTIVE' AND plan='SERVER_HLS' ORDER BY w.updated_at DESC` 无匹配索引。
- **触发频率与影响**：管理端轮询 30 次/分，但 ACTIVE 行只有个位数，排序成本可忽略。
- **建议修法**：不加索引（会增加高频更新表的写成本）。
- **风险**：无（不做）。
- **是否值得做**：**低（明确不做）**。

### I3 `transcoding_history` 的 COUNT（已读代码确认，不建议动）

- **位置**：`src/storage/sessions.rs:1274-1312`（`transcoding_history_page`）。
- **现象**：每次翻页 1 次 `COUNT(*)`；`ORDER BY finished_at DESC` 已有 `idx_transcoding_history_finished` 覆盖。
- **触发频率与影响**：管理端翻页时 1 次；表有 30 天保留（`sessions.rs:1744`），行数上界 = 30 天转码次数，万级以内。
- **建议修法**：不加索引、不改。
- **风险**：无。
- **是否值得做**：**低**。

### I4 `person_identities`（已读代码确认，无需改）

- **位置**：`src/storage/people.rs:612-640`（`resolve_or_create_canonical_person`）。
- **现象**：查找走 `PRIMARY KEY (provider, provider_id)`（`migrations/0073_canonical_people.sql:28`），
  按名匹配走 `idx_people_name`（`:12`），没有缺索引。
- **触发频率与影响**：每次演员解析 1 次键查（频率见 M2）；索引侧没有问题。
- **建议修法**：不改（问题在 M2 的调用次数）。
- **风险**：无。
- **是否值得做**：**无（索引已足够）**。

## 7. 建议的实施顺序

按"改动局部 + 可独立验证 + 不改变可见行为"优先：

1. **批次 1（零行为变化，收益确定）**
   - H1 的副产品：删掉 `reserve_playback_admission` 里重复的第二次计数（`sessions.rs:378`）。
   - H2 第 1、3 条：合并 session 投影、复用策略行（`session.rs:890-934`）。
   - M4：给 `collection_item_references` 加现值比较（`catalog.rs:7775-7853`）。
   - M2 方案 2：演员循环放进一个事务（`src/storage/people.rs` + `relations.rs` 签名）。
   - M5 方案 1：finish 与图片行同一事务。
   - S1：候选能力写入移到循环之后。
   - M3：`UPDATE media_items` 加 `IS DISTINCT FROM` 式条件（需要先确认 `updated_at` 语义）。
2. **批次 2（需要所有者确认"可能改变可观察行为"）**
   - H1：分片路径改"renew 优先、失败回落 reserve"（并发上限最长滞后一个心跳周期）。
   - S6：`pending_count` 改计数列或按需计算。
3. **批次 3（要有实测支撑再做）**
   - S5/I1 索引迁移（先在一份库副本上 `EXPLAIN QUERY PLAN` 对比）。
   - S2/S3/S4 的读复用、M8/M9、H3/H4/H5/H6（顺手做）。
4. **不做**
   - H7/I2（加索引反而增加高频更新表的写成本）、I3。

每个批次都要按第 2 节的口径给出前后数字（语句数、提交数、`catalog_outbox` 行增量），
不能只凭"看起来少了"。

## 8. 未验证项与推测汇总

| # | 内容 | 为什么是推测 | 怎么验证 |
|---|---|---|---|
| 1 | 清单请求 ≈ 每个分片 1 次（hls.js / Emby 客户端行为） | 没有抓包 | 浏览器 DevTools / 反代访问日志按 asset 计数 |
| 2 | 每路播放 ≈ 30 次写提交/分 | 由 1 推出 | 第 2.2 节的提交计数器 |
| 3 | 机械盘上 M2 的新演员写入是 30–60 ms/演员 | 未在 NAS 上测 | NAS 上跑"回填 10 部新电影"并计时 |
| 4 | 全库复扫 5 000 部电影 = 5 000 次 M4 提交 | 代码路径确认，实际条目分布未知 | `collection_item_references` 的 `dbstat` 差值与提交计数 |
| 5 | S6 的 `pending_counts` 聚合在 5 万条目下每秒 1 次 | 取决于页面是否订阅 `jobs` 事件、作业是否活跃；未采样 | 打开任务页跑回填，用 `EXPLAIN QUERY PLAN` + 慢查询日志 |
| 6 | M3 的 `catalog_outbox` 最多 5 行 | 引用 `docs/INDEX-AUDIT.md` §7 的静态分析，未复测 | 对 `catalog_outbox` 做写入前后计数 |
| 7 | 所有"读语句很便宜"的判断 | 未做每请求上下文切换测量 | 交接 §6 的方法（`powermetrics` / 计数器） |

另外两点需要后续确认，本审计没有答案：

- **`synchronous=NORMAL`（交接 W1）** 已实现为环境变量。H1/M2/M4/M5 的全部 fsync 成本估算都建立在默认 `full` 上；
  如果部署切到 `normal`，这几条的绝对收益会明显缩水（相对收益不变）。**在 NAS 上做完 W1 的 A/B 之前，
  不要用本文件的量级给用户承诺**。
- **PostgreSQL**：本机没有 PG 实例。M1–M5 的"事务次数"结论在 PG 上同样成立（每次 commit 一次 WAL flush），
  但 `BEGIN IMMEDIATE` 的单写者排队在 PG 上不成立，收益会小一些；所有 SQL 形状都只做了静态等价性核对。
