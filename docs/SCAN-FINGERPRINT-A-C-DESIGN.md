# LUX-561：A+C 扫描指纹方案与风险规避

状态：设计提案，尚未实施。2026-10-16 按项目所有者选择改用 A+C，取代先前的 B 主方案。工作分支：`codex/sha256-fingerprint-optimization`；运行时代码基线：`1e7c2ea8`。

## 决策

采用 **可靠 stat 快照命中时复用既有指纹；新文件或确认变化时计算 XXH3-128；旧 SHA 数据按需兼容**。固定数组和切片借用作为实现细节保留，不以 B 替代 A+C。

主要风险在于漏判变化、缓存与数据库不同步，以及格式切换导致整库失效。不能只把 `Sha256` 替换为 `xxh3`，也不能只比较 inode。

本次只更新设计和风险分析，不修改依赖、数据库或运行时行为。本次预计修改文件：本文档、`docs/SHA256-FINGERPRINT-OPTIMIZATION.md`、`docs/LUX-DEVELOPMENT.md`。

## 风险与处理规则

| 风险 | 可能发生的错误 | 处理规则 |
|---|---|---|
| inode 跨设备重复、被复用，或不稳定 | 把新文件认成旧文件；把不同路径的硬链接合并 | 以 root/path 定位行，比较 kind、device、inode、size、纳秒 mtime 和字段有效性；可用时加 ctime 作为保守的缓存失效提示。不能用 inode 作为唯一缓存键、授权依据或跨路径自动合并依据。 |
| 重命名/移动后 dev/inode 不变 | 跳过路径更新、父目录与版本归属处理 | 规范化相对路径和 root 作用域是短路条件。路径改变走现有移动/归属逻辑，不沿用旧路径的 cache hit。原有同目录版本身份复核不能省略。 |
| stat 字段不可用或精度不足 | 把未知的时间或 inode 当成 0 后误命中 | 有效性单独表示，未知不等于已知的 0。无法证明输入完整可靠时禁用 C，使用 A 的普通扫描路径。网络挂载、虚拟文件系统和重挂载需要单独验证；不能仅靠 OS 名称断言可靠。 |
| 内容改变但 size/mtime 保持 | stat 快照和“元数据摘要”均不变 | 这是当前 SHA 元数据指纹也存在的限制。ctime 只能增加线索，不能保证发现所有改写；要求内容可靠性时必须实际读取内容，不能靠周期性重算同一组元数据解决。见下文内容边界。 |
| 文件在发现与提交之间改变 | 把不同时间点的数据写成一份稳定结果 | 保留受控目录句柄、根身份检查和原有提交前二次 stat；变化则取消该条结果或按现有策略重试。缓存命中仅省摘要，不放宽 TOCTOU 防护。 |
| DB 被另一个扫描/worker 更新 | 旧 snapshot 覆盖新 fingerprint 或错误标记 seen | 沿用 entry ID、root/path、旧 fingerprint、missing 状态的 CAS；缓存还必须绑定这份旧 fingerprint 和完整 stat 输入。CAS 失败不能发布缓存或后处理结果。 |
| 把哈希当成绝对无碰撞的身份 | 稀有碰撞或人为构造的输入导致漏处理 | 选 XXH3-128，不截成 64 位；原始 stat 不相等时不能仅因 digest 相等就判 unchanged。它不是安全摘要；严格抵抗碰撞/ABA 的并发锚点应使用独立 source revision，而不是声称 128 位消除了风险。 |
| 新旧算法混用但无版本 | 所有老文件都判为变化 | 解析明确的格式，保持旧值直到真实变化；不能把“换编码”当“文件变了”。新旧混合库、旧任务与缓存冷启动都要测试。 |
| 全局替换共用 helper | NFO、人物关系和章节任务意外失效 | 新增专用扫描 v2 函数，保留旧 SHA helper。按消费者追踪 media fingerprint 与 NFO/content fingerprint 的用途，不批量替换所有 SHA。 |
| 为 C 每文件查一次数据库 | 省下摘要却增加大量 SQL/往返 | 接入现有按路径分批的 baseline 查询；复用原 discovery 写事务。暖缓存命中不更新 cache、不单独开事务。 |
| 扫描入口不一致 | 新扫描写 v2，旧入口再写 SHA，反复失效 | Manifest、直接扫描、增量与恢复路径统一通过版本分派；旧持久化 Manifest 保持自己的 workflow/指纹版本。兼容入口未完成前不能启用 v2 写入。 |
| 新二进制能读，不代表旧二进制能回退 | 回退后老程序把 v2 全当变化，甚至拒绝新 migration | 先交付双读兼容版本，再启用 v2 写入；回退到含新 migrations、支持双读的兼容构建。不能承诺任意旧二进制直接回退。 |

Linux 文档说明 inode 只在所属文件系统内唯一，ctime 也会因权限/所有者等变化而更新；一次 stat 的不同字段甚至可能来自调用期间的不同瞬间。因此以上条件是保守复用规则，不是内容未变化的数学证明。[inode(7)](https://man7.org/linux/man-pages/man7/inode.7.html)、[stat(2)](https://man7.org/linux/man-pages/man2/stat.2.html)。

## 快速短路的准确含义

短路省的是重新计算和分配 fingerprint，仍然需要本轮 readdir/stat 和有界数据库查询。不得直接依赖上次内存记录跳过文件系统观察，也不基于目录 mtime 跳过整棵子树。

命中必须同时满足：

1. 当前 root 可用、身份通过现有检查，路径及文件类型一致，条目不是 missing/reappeared。
2. 有一份完整且格式已识别的输入 snapshot；snapshot 所绑定的 fingerprint 与当前规范行 fingerprint 相同。
3. 本次 stat 的所有必要字段与 snapshot 精确相同，没有字段缺失、溢出或精度回退被伪装成相同值。
4. 没有明确的内容变更/强制复核请求，也没有尚未完成的错误补偿需要原流程继续处理。
5. 只跳过摘要阶段；媒体归属校验、旁车/任务补偿和 generation CAS 仍按原规则执行。

ctime 是缓存复用条件的加强项，不直接放进文件指纹：chmod/chown 引起 ctime 变化时，应先重新确认原指纹输入，不能直接触发 probe。ctime 是否变化也不能作为数据库事务版本号。

## 冷启动、升级与正常运行状态机

```text
readdir + 受控 stat -> 按路径批量加载 canonical baseline + snapshot
  |
  +-- 快照可靠命中 -> 复用原 fingerprint（可能是 SHA，也可能是 XXH3）
  |                  -> 原身份/补偿检查 -> 原 generation CAS
  |
  +-- 新文件 -> XXH3-128 -> 准备/二次 stat -> 原子提交索引和 snapshot
  |
  +-- 已有 v2 文件，快照未命中 -> XXH3-128 + 原始输入判定
  |                              -> unchanged 或真实变化处理
  |
  +-- 老 SHA 文件，快照缺失 -> 旧 SHA 验证一次
  |       +-- 与旧值相同 -> 保留旧值，只补 snapshot，不产生后处理目标
  |       +-- 确认变化 -> 生成 v2，按一次真实变化提交
  |
  +-- 已有可靠 snapshot 证明指纹输入变化 -> 可直接生成 v2
  |
  +-- 不支持/损坏的版本 -> 不短路，走明确的错误/保守处理策略
```

旧库第一次没有完整 snapshot，无法凭空获得安全的零计算升级。需要承受一次兼容验证与有界 snapshot 写入；之后稳定的旧 SHA 条目也能走 C，不必为改算法全部重写。缓存被清理或损坏只影响性能，不能改变正确性。

“强制使用 SHA 算一次，再 XXH3 算一次”只出现在需要确认旧数据是否变化的兼容路径，不应成为每次扫描的常态。旧 snapshot 已能证明必要输入变化时，不再重复旧摘要。

## 持久化建议：独立、有指纹锚点的 snapshot

当前 `filesystem_entries` 缺少 device，Manifest baseline 也没有完整 stat 输入；不能直接在现有查询结果上启用 C。建议用一个可丢弃的辅助表，不把业务真实性寄托在纯内存 cache 上：

| 建议字段 | 含义 |
|---|---|
| filesystem_entry_id | 主键及外键，删除条目时清理；查询仍由规范行的 root/path 限定 |
| snapshot_version | 快照编码版本，与指纹算法版本分开 |
| relative_path、entry_kind | 产生该指纹时的路径与类型，防止旧 snapshot 跟随移动行误命中 |
| stat_key | 固定编码的 size、mtime、device、inode 及各字段有效性；u64 原值不能静默截为 i64 |
| ctime_hint | 可选的高精度失效提示，缺失策略明确；不作为 digest 或事务版本 |
| fingerprint | 产生这份 snapshot 时的完整不透明指纹，必须与当前规范行匹配才可复用 |

此处为数据合同提案，正式 DDL/索引需在实施前核对 SQLite/PostgreSQL migration 序号、参数上限、数据库体积与查询计划。两种数据库必须一起迁移并支持空库创建。

- 读取在现有 batch baseline 查询里 LEFT JOIN；root/path、snapshot fingerprint 和版本的锚定在 SQL/映射层明确完成。不能多一次逐文件 SELECT。
- 新文件/变化文件：最终 stat 验证成功且规范行写入成功后，在同一个既有事务内 upsert snapshot。
- 老 unchanged 文件：CAS 成功后才补 snapshot。写失败整批沿用原回滚语义，或显式放弃缓存写入；不能把不完整 snapshot 标成有效。
- 暖命中：不为更新时间戳或“最近使用时间”重写 cache。seen generation 的既有写入不能算成新增 cache DML。
- 其他写入路径改变规范行后，旧 snapshot 的 fingerprint/path 锚点自然不匹配；完整性仍要求 audit 所有 writer，不能假定任意 writer 都能保证 size/mtime 与 fingerprint 同步。
- 不把整个库装入内存；只保留当前有界 batch。清理随条目删除或单独的有界维护完成，不创建每轮全表清理。

## v2 格式与 Rust API 注意事项

选用 **XXH3-128**，算法固定 seed=0。不能经 `std::hash::Hasher::finish()` 取值，那是 64 位接口；使用 `xxh3_128` 或 `digest128()`。流式和 one-shot 必须对相同规范字节产生相同结果，并用 golden vectors 固定。

建议格式：旧值仍识别为 32 字节 raw SHA-256；新值是 `b"LUX-FP-2\0"` + 16 字节 XXH3-128 大端输出，共 25 字节。必须同时校验新格式的长度和标记，不用补零假装成 32 字节。算法输入使用独立域标记、明确的路径 UTF-8 字节长度、路径、类型和固定宽度 stat 字段，Option 的有效性位与数值分开；不得使用 Rust 的内存布局、DefaultHasher 或本机字节序。

解析时先按完整长度识别：32 字节视为 legacy SHA，即便其随机前缀恰好像标记；只有恰好 25 字节且标记匹配才是 v2，其余值不猜测算法。所有摘要消费者把这个扫描指纹当不透明 token；未知算法不得当 v2 解释。数据库 BLOB 本身可容纳变长值，但现有测试中的 `LENGTH(fingerprint)=32` 和任何格式假设必须逐项审核。

避免全局替换 `compute_file_fingerprint`：`metadata.rs::nfo_fingerprint_from_stamp` 复用它，而 NFO/人物缓存存在 32 字节假设。保留其原合同，新增 `compute_scan_fingerprint_v2` 和集中版本分派，并检查：

- `scanner.rs`、`scanner/library.rs`、`scanner/library/prepare.rs` 与 `scanner/jobs/manifest_discovery.rs` 的所有入口；
- `storage/jobs/scan/manifest/discovery/presence.rs` 和 `apply/deltas.rs` 的历史 CAS 值；
- `chapter_detector.rs` 的 source/input fingerprint 与旧任务恢复；
- `storage/media.rs::find_items_by_source_fingerprint` 以及人物关系恢复读取的旧引用。

xxHash 官方保证定版输出跨平台稳定，但明确它是非加密哈希；这里只用于媒体扫描，密码、HMAC、插件包完整性与内容安全摘要继续使用各自现有算法。Rust streaming hasher 有较大的内部缓冲，短输入不保证它最省成本：one-shot 可复用 batch 私有的序列化缓冲；不得逐字段拼出多个堆对象，也不引入共享全局锁。[xxHash 官方说明](https://github.com/Cyan4973/xxHash)、[Rust XXH3 API](https://docs.rs/xxhash-rust/0.8.19/xxhash_rust/xxh3/index.html)、[streaming API](https://docs.rs/xxhash-rust/0.8.19/xxhash_rust/xxh3/struct.Xxh3.html)。

## 碰撞和并发的边界

XXH3-128 能把非恶意输入的偶然碰撞风险降得很低，不能提供对抗性碰撞保证。完整 stat 判等先于 digest 判等，避免把已知不同的元数据只因相同 digest 合并。

现有代码也把 fingerprint 用作 CAS。迁移至少保留原 CAS 条件，不能变成“新 digest 等于旧 digest 就允许写”。如果要求在摘要碰撞或 A→B→A 的更新中仍严格区分每次 source 更新，应引入单调 `source_revision`：真实输入更新在同一事务递增，下游任务以该 revision 作为 freshness/提交锚点；generation、秒级 updated_at 和 ctime 都不能替代它。该强化会扩大 writer/consumer 改动范围，需先拆为独立验收增量；不能在尚未实现时声称 A+C 已有此保证。

本提案不把新增 source revision 隐含成已经完成的工作，也不以概率数字替代并发回归。

## 内容改写和不可靠文件系统

当前 SHA 与提议的 XXH3 都是在哈希元数据；如果原地改写后路径、device、inode、size、mtime 都恢复，换哪种哈希都不能检测到差异。

建议默认目标是保持当前检测能力，同时用可靠的 ctime/事件提示保守地禁止 C。若需要更强能力：

- 对 `.strm`、NFO 等小文件，可在既有大小上限内读取内容后做独立的内容校验；只在后台运行，并单独测量增加的 I/O。
- 对大媒体，可选择有界片段抽查，必须注明可能漏掉未采样区域；要求完整性保证时需完整读取或可信的文件系统 change counter。
- 明确的内容变更事件应进入强制复核流程；只禁用 C 然后重算同一个元数据 digest 仍然不够。事件漏报/网络 stat 缓存还需靠实际内容核验或可信 change counter，不能由 hash 算法解决。

这些加强项需要单独验收，不能默认把“全量扫描”宣传成完整内容校验，也不在本次设计阶段触发生产内容读取。

## 实施顺序与验收门

按顺序推进，实施前把每个增量的精确文件清单控制在 5 个以内；涉及新依赖、snapshot 表和格式的增量先定合同，再 TDD：

1. 先完成 v1/v2 双读与纯算法 golden tests，保留 v1 写入，所有扫描入口和消费者准备好后才能启用新格式。旧 Manifest 的 v1 observation 与新规范行 v2 值不等不能直接判为内容变化；必须经过版本感知复核，无法安全对齐的旧 observation 走原 UNSTABLE/安全重试路径，不盲写回 v1 或改写旧 CAS 锚点。
2. 增加两种数据库的 snapshot migration、批量读取和原子写入；关闭 C 也应正确工作。
3. 分离 raw stat observation 与 fingerprint 生成，把 baseline 查询移到决定是否哈希之前；先打通 Manifest，再覆盖其他扫描入口。旧 workflow 不强制换格式。
4. 打通冷库、暖库、增量和恢复路径；每个实现增量跑最窄 scanner/扫描任务/storage target。新增完整性行为若超出规格，先写清验收再实现。
5. 对比 SHA 基线、A、C、A+C，记录 CPU/墙钟、摘要计数、snapshot SQL/DML、DB 体积、RSS、前台 p95。冷升级和暖重扫分别报告，完成 Rust 全量门后再讨论部署。

关键回归：

- 混合 SHA/v2 库升级后，旧 unchanged 文件不被重新 probe/NFO；缓存预热后稳定目标不调用 SHA 或 XXH3。
- 新文件/真实变化使用 v2；ctime-only 变化不直接触发昂贵后处理；损坏 snapshot 走 fallback。
- 跨设备同 inode、同 inode 不同路径、硬链接、移动、删除重建、纳秒变化、缺字段、溢出和重挂载。
- 发现后文件改变、并发 DB 更新、CAS 失败、取消/事务回滚、缺失/reappeared、根身份失效。
- 人工注入相同 digest 的不同 stat，确认不走 unchanged；若交付 source revision，另测 ABA 和过期 worker 不能提交。
- 旧持久化任务和新任务共存；未知版本不误解析；兼容构建回退；NFO/人物/章节的现有合同不意外改变。

所有 Cargo 命令先确认 `/Volumes/Toshiba` 挂载，并设置 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target`。当前设计阶段不运行产品实现测试，不把未实施的验收项标为通过。

## 性能表述

此前 SHA 微基准在 ARM64 上每次约 0.22–0.94 微秒（60k 次约 13–57 ms）；尚未对本设计测 XXH3 或真实扫描。因此“微秒级”是摘要内核的测量单位，不能直接当作每文件扫描耗时，也不能证明 A+C 显著降低 NAS CPU。

最终成功标准是：可靠暖命中确实零摘要、兼容升级不制造全库变化、批量快照读写不会吞掉节省的成本，并且没有可重复的端到端退化。摘要更快但数据库更慢的结果不能算优化完成。
