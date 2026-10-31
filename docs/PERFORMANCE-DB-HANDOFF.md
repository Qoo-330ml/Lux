# 性能与数据库优化交接清单

日期：2026-10-30。基线：`test` 分支，包含第一轮结构与代码质量优化的全部已合并成果。
状态：**本文件列出的条目全部尚未实施**（除"第九节"明确标注的）。本文件只记录问题、证据、建议和约束，
不改变产品规格；实施前仍需按 `CLAUDE.md` 在 `docs/LUX-DEVELOPMENT.md` 登记 `LUX-*` 任务。

## 0. 先读这个

### 0.1 可信度

- 本文的语句数、提交数、唤醒次数多为**读代码得到的静态估算**，没有运行时采样。项目自己也承认缺少每请求 SQL 计数
  （`docs/PERFORMANCE.md` 的"SQL / 请求（现状记录）"一节）。**所以第一步是给 `Database::query*` 加每请求计数**（见第八节）。
- 行号是 2026-10-30 当天的快照，后续重构会偏移，请用函数名重新定位。
- 本机是 arm64 开发机，任何数字都**不能外推到 NAS（机械盘 / 低功耗 x86）**。
- 本机没有 PostgreSQL，涉及 PG 的 SQL 只能做静态等价性核对。

### 0.2 约束（来自项目规则与已踩的坑）

- 集成测试已按领域分组：`cargo test --locked --test <group> <module>::`，旧文件名映射见 `docs/TEST-TARGETS.md`。
  只在任务完成或阶段门禁时才跑 `cargo test --locked --all-targets`，并加 `--no-fail-fast`
  （不加的话，一个 target 失败会让后面的 target **不再运行**，看起来像"全过"）。
- `tests/**` 里的冻结测试不得修改（约 3 处 `AppState::default()` + 8 处 `app()` 构造点）。
- 不得在请求路径里做全库扫描、TMDb、NFO 解析、`ffprobe`；不得在 Tokio 核心线程上做阻塞文件系统或进程工作；
  不得记录凭据、令牌、cookie、完整外部 URL。
- 行为要保持一致：以下优化里凡标注"会改变可见行为"的，必须先由项目所有者确认。

### 0.3 本机已知的"假失败"测试

- `admin_settings::transcoding_settings_are_admin_only_validated_and_persisted` 与
  `web_playback::broken_hardware_encoder_falls_back_to_software_transcoding`：对 `transcodingStatus.available` 的期望互相
  矛盾，在装了 VideoToolbox 的开发机上必有一个失败，与优化无关。
- `web_static::*`：需要先 `pnpm --dir web build` 生成 `web/dist`。
- 并发高负载时会随机失败，**单独串行跑稳定通过**：登录请求超时（argon2 在负载下）、`danmaku::…`、
  `image_writer::manual_scraper…`、`media_info_config_api::…`。

## 1. 播放进度上报（最大的热点）

Emby 客户端与 Lux Web 播放器每 10 秒上报一次进度（`web/src/features/player/usePlaybackReporting.ts` 的
`PROGRESS_REPORT_INTERVAL_MS = 10_000`）。以非管理员播放一集剧集为例：**读语句约 20 多条，写提交约 7 次**（估算）。
电影路径更少，因为 P2、P3、P4 只对剧集执行。

主流程：`src/api/playback.rs` 的 `handle_emby_playback_event`（约 1368 行）与 Web 侧的 `lux_post_progress`（约 4770 行）。

| # | 问题 | 位置 | 建议 |
|---|---|---|---|
| P1 | 每个 tick 都写 `playback_sessions` 与 `user_item_state` 两张表 | `src/storage/sessions.rs` `record_playback_event`（约 530 行） | 必要，但可合并写入，见第五节的"进度写合并" |
| P2 | 每个 tick 都执行 `refresh_user_series_next_for_item`：SELECT + DELETE + 窗口函数 INSERT；DELETE 与 INSERT 是两次自动提交，中间存在读到空指针的窗口 | `src/storage/user_series_next.rs`（约 395、415 行） | 只在开始、暂停、停止、`is_played` 翻转时执行；改成单条 UPSERT |
| P3 | 每个 tick 都执行 `sync_played_container_states`：聚合整个系列的所有剧集，并对季和系列行 upsert，值没变也写页 | `src/storage/sessions.rs`（约 654 行） | `record_playback_event` 已算出 `auto_played`，让它返回"是否翻转"，只在翻转时调用 |
| P4 | P3 末尾又刷新一次同一系列的 next 指针，与 P2 重复；同一集的 `series_id` 也被查两次 | 同上 | 合并成一次 |
| P5 | 每个 tick 独立 UPDATE `playback_admissions`，一次独立写提交 | `renew_existing_playback_admission`（`sessions.rs` 约 401 行） | 节流到秒级，或并入 P1 的事务 |
| P6 | 只为取 duration 就调用 `catalog.find_item`（完整详情加载，约 5 条以上查询） | `src/api/playback.rs` `emby_playback_event_duration_ticks`（约 1890 行） | 优先用 `previous_session.duration_ticks`，没有再走 catalog；注意代码注释要求保持"以目录时长为准、不被客户端的部分时长覆盖" |
| P7 | 会话内每次重复校验访问权限；`find_item_library_id` 同一请求里调用 2 次（handler 一次、`can_view_item` 一次） | `handle_emby_playback_event` 与 `src/application/access.rs` `can_view_item`（约 70 行） | 按 `(user, play_session)` 缓存约 60 秒（权限被撤销时最长 60 秒滞后，需确认可接受） |

预期效果：普通 tick 从约 7 次写提交降到 1 到 2 次，读从 20 多条降到约 3 条。

## 2. SSE 广播放大（已读代码确认，不是估算）

`UserEventScope::Home` 没有用户维度（`src/application/admin_events.rs`，`publish_home_coalesced` 约 200 行）。
播放上报每次都调用它，事件广播给**所有** SSE 连接；前端（`web/src/features/activity/useUserEvents.ts`）
收到后让 10 类查询键（home、libraries、item、item-images、playback、children、favorites、collection、person、library）
全部 refetch。

结果：N 个用户在线，一个人播放，其他 N-1 个人的浏览器每约 10 秒都会刷新首页和相关查询。

建议：事件带上 `user_id` 与 `itemId`，SSE 层只推给对应用户；前端对 progress 类事件只刷新 continue-watching
和对应条目的 playback。这一项几乎不改变可见行为，收益大、风险低，建议与第一节一起先做。

## 3. 鉴权与访问控制（每个请求都付）

| # | 问题 | 位置 | 建议 | 注意 |
|---|---|---|---|---|
| R1 | 每个请求都重新鉴权，无任何缓存。Web 约 2 次查询（会话 + 规则）；Emby 约 4 次（令牌、IP 规则、设备、设备规则）。另有 `touch_access_token`（已有节流） | `src/auth/sessions.rs` `resolve_principal`（约 248 行）；`src/auth/emby.rs` `resolve_token_scoped`（约 377 行） | 令牌哈希到主体的短 TTL 缓存（5 到 10 秒），把 Emby 的 4 次合并成 1 次 JOIN | **撤销令牌、禁用用户、改访问规则必须显式即时失效**，不能只靠 TTL |
| R2 | 访问上下文每请求现查：`accessible_library_ids` 约 20 个调用点，内容限制约 15 个，均无缓存 | `src/application/access.rs` | 按用户的 `AccessContext`（媒体库 + 内容限制 + 版本号），用户权限变更时递增版本 | 同上 |
| R3 | 图片请求逐张鉴权并查库，还对每个候选反复 `canonicalize`/`stat`；`metadata_root` 每次都重新 canonicalize | `src/application/images.rs` `resolve`（`resolve_candidates` 约 2453 行）；`src/storage/catalog.rs` `list_item_image_candidates`（约 9089 行） | 按 `(image_id, content_tag)` 缓存解析结果；`metadata_root` 启动时算一次；**路径校验要保留**（CLAUDE.md 要求 canonicalize 并限制在库根内） | 仅首次加载受益，浏览器缓存命中后无影响 |

## 4. 读路径

| # | 问题 | 位置 | 建议 |
|---|---|---|---|
| R4 | 列表行结果集过大：每个条目都 LEFT JOIN `media_sources` 与 `media_streams`（含 `details_json`），外加 4 个相关子查询取海报、剧照、缩略图、Logo | `src/storage/catalog.rs` `list_catalog_rows`（约 3067 行）、`fetch_catalog_rows_internal`（约 3472 行）及同族查询 | 网格用摘要投影；详情和 Emby 的 `Fields=MediaSources` 才联结流；海报 tag 可冗余到 `media_items`。注意 `CatalogListRow` 里 `series_name`、`source_relative_path`、`resume_total` 是可选列（`#[sqlx(default)]`），不同查询的投影不同 |
| R5 | 首页只缓存推荐一块；继续观看、最近添加、各库最新、库列表每次都查。文档记录首页 CPU 277.5 ms、墙钟 107 ms（66k 夹具，读模型 off） | `src/application/home.rs`（`CachedHomeSnapshot` 约 85 行） | 4 个 rail 一并放入 per-user 缓存，用 per-user generation 失效 |
| R6 | 用户状态变化触发全局缓存失效：`home.invalidate()` 对所有用户 bump 同一个 generation 并清空库页缓存，后台还会重算最多 64 个条目 | `src/application/home.rs` `invalidate_now`（约 373 行） | 拆成"目录级（全局）"和"用户级（只失效本人）"两种；用户改库排序、改偏好不该让别人的缓存失效 |

## 5. 写入侧

| # | 问题 | 位置 | 建议 | 注意 |
|---|---|---|---|---|
| W1 | SQLite 使用默认 `synchronous=FULL`。WAL 模式下 NORMAL 不会损坏数据库，只可能在断电时丢最后几次提交 | `src/storage/repository.rs` 的连接 `after_connect`（约 1162 行，只设了 `foreign_keys`、`journal_mode=WAL`、`busy_timeout`、`journal_size_limit`）；删除任务已临时用 NORMAL（`library_deletion.rs`） | 对小事务多的播放写路径收益最大，机械盘上尤其明显 | **必须在 NAS 上做 A/B**；文档里的 A/B 只测了批量扫描（批量提交已把 fsync 压到很少），不代表小事务 |
| W2 | 读写共用一个 8 连接池；已有 `BEGIN IMMEDIATE` 与 50 ms 重试补丁 | `SQLITE_DATABASE_POOL_MAX_CONNECTIONS = 8`（`repository.rs` 约 390 行）；`begin_metadata_write_transaction`（约 1305 行）；`storage/users.rs` 的 `SQLITE_USER_UPDATE_RETRY_*` | 拆成"1 个写连接 + N 个只读连接"，可去掉 `SQLITE_BUSY_SNAPSHOT` 的补丁 | 需要先有统一的访问层入口；`Database` 拆分试点结论是不全面推广，见第九节 |
| W3 | UPDATE 几乎没有"值未变则不写"保护：312 条里只有 21 条带 `IS NOT` 之类的保护 | `media_items` 18 条、`media_sources` 14 条、`filesystem_entries` 15 条 | 重扫、元数据回写这类大概率无变化的路径加保护条件 | SQLite 的 ON CONFLICT 即使值相同也会写页并触发触发器 |
| W4 | `media_items` 写放大：约 18 个索引、6 个触发器 | `migrations/` | 用 dbstat 或查询计划找出无人使用的索引，逐个删除 | 需要实测；删索引要走迁移，且 PG 迁移要配对（`scripts/check-migration-parity.py`） |
| W5 | `user_series_next` 先 DELETE 再 INSERT | `src/storage/user_series_next.rs` | 改成单条 UPSERT，找不到下一集时才 DELETE | 与 P2 同一处。已实施（提交 `97fcf4ab` 测试、`388b66cc` 实现）：刷新改为「先读候选，再一条 `INSERT … ON CONFLICT(user_id, series_id) DO UPDATE … WHERE 值有变化`（`IS DISTINCT FROM`，SQLite/PG 同写法）」，只有候选为空才 DELETE；`rebuild_user_series_next` 也移进一个事务、只写有变化的行、只删候选集里没有的行（原来是事务外全表 `DELETE` 再重插，整表存在空窗）。无 schema 变更、无迁移。PG 只做静态核对（本机无 PostgreSQL）。代价：每条刷新多一次候选读取（2 条语句变 3 条），`rebuild` 多一次候选集计算。 |

**进度写合并（会改变可见行为，必须先确认）**：位置 upsert 在内存里合并，每 30 秒或状态变化时落盘。代价：崩溃最多丢约
30 秒进度；其他设备的"继续观看"位置晚到几十秒。

## 6. 后台任务唤醒

第一轮已把这一项做成了盘点与实测，完整数据见 `docs/BACKGROUND-TASKS-AUDIT.md`。要点：

- 静态估算空闲唤醒约 **476 次/分**，前 4 项占 88%：本地元数据 outbox 兜底 250 ms（240 次/分）、`catalog_items` 增量 1 s、
  搜索重建 1 s、全局合集同步 1 s（各 60 次/分）。
- 空闲 3 分钟实测：CPU 约 0.5 到 0.8% 单核，数据库文件零写入。
- **实测上下文切换反推的唤醒次数是静态估算的 20 到 40 倍**，约三分之二可由"每次查询经过 sqlx worker 线程往返"解释，
  其余未归因。建议先在 NAS 上用 `powermetrics`（需要 root）或加迭代计数器再定论。
- 文档里的 A 类零风险调整（预计 476 → 约 60 次/分）：outbox 兜底改 5 到 10 s、catalog/搜索改 5 s 或写侧 Notify、
  合集同步改 5 s、播放会话回收和邮件/webhook 改事件驱动。**这些会改变读模型、搜索、合集的新鲜度语义，需要先确认。**
- 另有 8 个后台循环没有保存 `JoinHandle`，关停时不能被 drain。

## 7. 其他线索（未验证或已评估后决定不做）

| 线索 | 说明 |
|---|---|
| 读模型默认关闭 | `LUX_READ_MODEL_MODE` 默认 `off`（读取点在 `src/application/catalog.rs` 约 103 行）。读模型已实现；文档里首页读模型的 A/B 数据是 CPU 密集的主要出路。是否默认打开需要决定，并要在 NAS 上实测 |
| 库页缓存刷新走慢路径？（**未验证**） | `src/application/catalog.rs` 约 304 行，库页缓存后台刷新服务构造时硬编码了 `read_model_mode: ReadModelMode::Off`。即使环境变量设为 `on`，后台刷新可能仍走慢路径。当时没有深究，需要确认是不是有意的 |
| 管理员批量操作的 N+1 | `user_account_preferences`、`user_permission_copy`、`user_presets` 的循环逐个查。第一轮评估后**决定不改**：循环体是读后写，需要保留每用户事务和错误隔离。若要降到常数，需新增 `storage/users.rs` 的批量读接口（`find_users_by_ids` 与偏好版本批量读） |
| 旧身份键修复逐集查询 | `repair_legacy_identity_keys`（`src/application/scanner/library.rs` 约 426 行）首次执行 5 万条语句、约 3.4 秒，在后台跑不阻塞监听，已有完成标记，只在首次升级时付一次。可考虑批量化 |
| HLS 转码、刮削、元数据写入的数据库行为 | 第二轮**没有覆盖**，值得单独审计 |
| 搜索 | 文档记录搜索 CPU 21 ms，成本在"匹配 5.6 万行 + 计算总数 + 排序"那条查询，不在 DTO 装配 |

## 8. 建议的推进顺序

1. **阶段 0：建立度量。** 给 `Database::query/query_as/query_scalar` 加每请求的 SQL 条数与写提交计数
   （任务局部计数器 + 中间件），暴露在管理员性能端点或测试钩子里。之后每一步都要有前后对比。注意 `#[cfg(test)]` 下已有
   `query_count()`，可参考但它不覆盖生产路径。
2. **阶段 1：播放 tick 条件化（P2 到 P7）+ SSE 用户维度（第 2 节）+ `user_series_next` 改 UPSERT（W5）。**
   读写大幅下降、基本不改变行为。
3. **阶段 2：W1（`synchronous=NORMAL`，NAS 上 A/B）与 R3（图片解析缓存）。**
4. **阶段 3：R1、R2（鉴权与访问上下文缓存，失效必须即时）、R5、R6。**
5. **阶段 4：R4（列表摘要投影）、W2（读写池拆分）、W3（UPDATE 保护）、W4（索引审计）。**
6. **阶段 5（需所有者确认）：** 进度写合并；后台唤醒的 A 类调整（会改变新鲜度语义）。

每个阶段都先写失败测试再改，改动后用阶段 0 的计数证明收益；缓存类改动的基准要贴近真实工作集。

## 9. 与第一轮成果的关系

- **已合并、与本文有交叉的：**
  - A2（PostgreSQL 占位符改写缓存）：只覆盖 PG 的 `?` 到 `$n` 改写，与上面的问题无重叠。真实形态（约 1100 条静态语句 × 8 线程）约 4.7 倍。
  - C3（阻塞调用审计，`docs/BLOCKING-IO-AUDIT.md`）：播放路径上没有新增阻塞问题。
  - C2（后台任务盘点）：是第 6 节的来源。
  - B1（`Database` 拆分试点，`docs/DATABASE-SPLIT.md`）：结论是**不全面推广**，仅对新代码采用；仓库对象无法参与调用方事务，对 metadata/media/catalog 这类事务密集的域风险高。W2 如果要做，需要考虑这一点。
- **未做、但和本文有关的第一轮遗留：**
  - B4 行解码：存储层按列名取值约 1500 → 546 处，未做完的主要是 `catalog.rs` 约 179 处、`jobs/scan/manifest/state.rs` 55、`database_diagnostics.rs` 34 等。属于 panic 面清理，不影响性能。
  - 方案 B（setup 与业务路由分离）仍在进行，与性能无关，但改动 `src/api` 的路由与 `AppState`，新会话如果也改 `src/api/**` 要注意冲突，先看 `docs/SETUP-READY-SPLIT-DESIGN.md` 与 `git log`。

## 10. 未验证项汇总

- PostgreSQL 实机：B4 各批与 setup 恢复的新 SQL 仅静态核对，建议跑 `cargo test --locked --test postgres_database -- --ignored`
  与相关 `#[ignore]` 用例。
- Docker 与 NAS：setup 完成后的真实 `exec` 重启只在本机裸进程用 `scripts/restart-recovery-smoke.sh` 验证过；
  `disk-write-fault`、`mount-loss`、`proxy`、`probe` 四个冒烟脚本本机没有 Docker，只做了 `bash -n`。
- 所有性能数字均来自 arm64 开发机。
