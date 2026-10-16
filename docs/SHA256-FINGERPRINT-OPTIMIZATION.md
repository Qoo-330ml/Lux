# LUX-561：SHA-256 扫描指纹优化设计

> 本文 B 主方案已由项目所有者于 2026-10-16 选择的 [A+C 方案](SCAN-FINGERPRINT-A-C-DESIGN.md) 取代。下文保留为原方案比较和 SHA 微基准记录，不作为当前实施指令。

状态：设计提案，尚未实施。日期：2026-10-16。基线：`test@1e7c2ea8`。

## 决策

三个建议中，当前选择 **B：固定数组返回值**。保留 SHA-256 和 `LUX-FP-1` 编码，把扫描期间新计算的临时指纹保存为 `[u8; 32]`，需要借用时传 `&[u8]`，确实需要动态所有权的旧结构才转换为 `Vec<u8>`。

这是一次消除分配和深拷贝的有限优化。它不减少 SHA-256 压缩轮数，也不承诺显著降低整机 CPU。当前证据不足以把第二点认定为 300% CPU 的主要来源；不得直接采用分析报告中“节省 2–5 秒”“降低 5–10% CPU”的数字。

本次交付只创建分支、核查代码和编写方案，不修改运行时实现。实施范围限于第二点，不调整 ffprobe、全局并发、扫描事务或其他哈希用途。

## 已核对的事实

| 位置 | 当前行为及设计影响 |
|---|---|
| `src/application/scanner.rs::compute_file_fingerprint` | 输入是相对路径、size、纳秒 mtime、device、inode；不读取媒体内容。函数返回 32 字节 `Vec`，每次 `.to_vec()` 分配。 |
| `src/application/scanner.rs::manifest_entry_observation*` | 每个 stat observation 都立即计算指纹；根目录和目录 observation 也使用同一函数。 |
| `src/application/scanner/jobs/manifest_discovery.rs` | 文件指纹在批量 baseline 查询之前已经计算；命中后又把指纹克隆到 seen-entry。仅改变函数返回值而马上 `.to_vec()`，不能优化这条主路径。 |
| `src/application/scanner/manifest.rs` | 二次 stat、根路径身份检查和提交前复核用于防止文件/挂载变化；不得为省一次摘要删除这些检查。 |
| `src/storage/jobs/scan/manifest/discovery/presence.rs` | 指纹参与 generation 更新的 CAS 条件；同 ID、同路径、同指纹且非 missing 才标记 seen。 |
| `src/storage/jobs/scan/manifest/apply/deltas.rs` | 历史 `base_fingerprint` 是差异应用的 CAS 锚点，不只是可随意替换的缓存。 |
| `src/storage/repository.rs`、`src/storage/filesystem.rs` | 规范 filesystem entry 未保存 device；Manifest baseline 当前只读取 ID、kind、fingerprint、missing 等字段，没有完整 stat 快照。 |
| `src/application/metadata.rs::nfo_fingerprint_from_stamp` | NFO stat 指纹复用公开计算函数；直接改算法也会改变这一用途。 |

规格依据是 `docs/LUX-DEVELOPMENT.md` §12.3、§12.4、LUX-040、Manifest 合同和 ADR-045。当前方案只改变内部临时值的表示，不改变指纹输入或判等规则。

## 三个方案的比较

| 方案 | 能省什么 | 当前代价与风险 | 结论 |
|---|---|---|---|
| A：xxHash3/FxHash/AHash | 可能降低每次摘要计算成本，首扫和重扫都可能受益 | 新算法与旧 SHA 字节不相等，会触发变化判定和下游失效；需要算法版本、兼容比较、迁移与回滚设计。还涉及共用计算函数的 NFO。不能把非加密摘要笼统视为“无需考虑碰撞”。 | 本次不选。若未来实际 profile 证明摘要计算占比显著，再单独评估固定格式的 XXH3-128；不把 FxHash/AHash 当作可互换的持久格式。 |
| B：`[u8; 32]` | 省去临时摘要的堆分配和部分 clone 深拷贝 | SHA 运算仍存在；如果调用方立刻转回 Vec，收益消失。内部结构和 SQL 借用边界需要一起调整。 | **本次选择：收益有限但确定，兼容成本最低。** |
| C：stat 元组快速判等，必要时再算 SHA | 在可靠缓存命中时避免重复摘要 | 必须包含路径和完整身份条件，inode 不能独立作为身份依据。现有持久数据不够，需要完整快照及其与旧 fingerprint 的一致性锚点；新增读取、写入和缓存维护可能比摘要本身更贵。首扫仍需计算。 | 暂不引入。不能把不完整的 tuple 判等作为现有 fingerprint 的等价替代。 |

不把三个方案叠加：B 的验证结果应独立可归因；A/C 只有新的实测证据才能成为后续工作。

## 探索性微基准

环境：macOS `arm64`，Rust `1.97.1`，独立最小 crate，固定 `sha2 = 0.10.9` 默认 features，Cargo release 优化。使用独立锁文件，不修改 Lux 的 Cargo.toml/Cargo.lock。Cargo 输出位于 Toshiba 共享 target。

每组 60,000 个预先生成的路径，路径字节长度分别为 64、256、512；连同域标记、长度和 stat 字段，实际哈希输入长度分别是 113、305、561 字节。每组 11 轮，剔除第一轮，之后按相反顺序交替运行 Vec/array，使用 `black_box` 消费输入和结果。计时不包括路径生成、stat、SQL、任务调度或服务启动。

| 路径字节数 | Vec 中位数 | 数组中位数 | 中位数差值 | Vec 最小–最大 | 数组最小–最大 |
|---|---:|---:|---:|---:|---:|
| 64 | 13.408 ms | 12.373 ms | 1.035 ms | 12.921–14.560 ms | 12.006–14.821 ms |
| 256 | 31.478 ms | 30.469 ms | 1.010 ms | 31.251–32.429 ms | 30.201–31.011 ms |
| 512 | 56.152 ms | 54.999 ms | 1.152 ms | 55.676–57.175 ms | 54.574–55.729 ms |

这些是合成内核的探索结果，分布存在重叠，不作为端到端提速百分比的承诺。它们只支持“临时 Vec 有小成本”和“当前机器上单次 60k 摘要不是数秒”这两个判断。未测 FNOS、Linux/x86_64、真实媒体路径分布或生产 CPU 占比。

首次尝试直接链接共享 target 里的已有 rlib，其编译配置来源不充分，结果已排除；表格只使用独立 crate 重新编译并由 Cargo 确认为 optimized 的最终运行。最终两种函数分别完成相同编码和摘要，避免 Vec 版本额外经过数组函数的薄包装调用。

可复现的内核代码及命令见文末。正式验收仍须用实际 Lux 构建做基线/分支交替比较，并记录实际分配减少量。

## 优化后的数据流

```text
已有路径规范化与 stat
  -> 原字节顺序的 SHA-256 -> [u8; 32]
  -> Manifest observation / 二次校验 / seen-entry 传递固定数组
  -> SQL bind 借用 &[u8]
  -> 旧业务结构确需 Vec 所有权时才 to_vec()
```

### 1. 计算函数与公开兼容边界

- 在 `scanner.rs` 增加内部 `compute_file_fingerprint_bytes(...) -> [u8; 32]`，使用 `hasher.finalize().into()`，不拼接临时输入 Vec，不使用共享可变 hasher。
- 保持现有公开 `compute_file_fingerprint(...) -> Vec<u8>` 签名，作为调用内部函数再转 Vec 的兼容包装。避免为一次性能改动改变公开 Rust 接口及所有调用方。
- `current_file_fingerprint` 改为返回 `(String, [u8; 32])`；仅需要比较或向 storage 借用的扫描路径直接使用内部函数。
- `library/prepare.rs` 中真正需要构造 `NewMovieFile`、`NewEpisodeFile` 等 Vec 字段的路径保留一次所有权转换；这些分配不能算成已经消除。
- NFO stat 指纹继续使用兼容包装。NFO 内容 SHA、语义 SHA、插件校验、认证和其他 SHA 用途不在本任务内。

### 2. 打通 Manifest 主路径

- 把内部 `NewScanManifestEntry.fingerprint` 改为 `[u8; 32]`。`manifest_entry_observation` 和 `manifest_entry_observation_from_stat` 直接产生固定数组。
- 把 `NewScanManifestSeenFilesystemEntry.fingerprint` 同样改为固定数组，命中分支按值复制，避免刚进入 seen-entry 又分配 Vec。
- 二次 stat 及 observation clone 保留原调用时序；摘要字段的 clone 变为 32 字节复制。
- observation 转入现有动态所有权结构时显式 `.as_slice().to_vec()`，使必要的分配可见；不扩大到所有存储 DTO 或媒体模型。
- SQL bind 一律借用 `.as_slice()`；不能为适配 SQLx 在绑定处新增 `.to_vec()`。数据库读取结果和历史 CAS baseline 仍为 `Option<Vec<u8>>`，与新值通过字节切片比较。

### 3. 历史数据与异常长度

数据库格式继续是原有 32 字节 BLOB，不新增 migration、算法列或运行时配置，不回填、不触发全库重算。

`StoredScanManifestDelta` 读取出的 Vec 转换成内部 observation 时检查长度。合法 32 字节原样复制；非 32 字节不能截断、补零或 panic。保留原“不稳定 observation 不提交正向索引”的结果，走已有 `Unstable` 路径，并添加回归确认。

NULL、空值和任意长度的旧 `base_fingerprint` 继续原样保留用于 CAS；不能因内部固定数组把这些旧锚点改写成零数组。测试中的合成根 observation 可以改用明确的 32 字节 fixture，但异常持久数据测试必须继续保留。

### 4. 必须保持的字节合同

顺序严格不变：`b"LUX-FP-1\0"`、UTF-8 路径**字节**长度的 u64 小端序、路径字节、i64 size 小端序、i64 mtime 小端序、u64 device 小端序、u64 inode 小端序。

- `None` 继续按现有的 0 编码，与 `Some(0)` 保持原来的等价关系。
- 路径规范化、非 UTF-8 拒绝、纳秒精度、溢出和异常时间处理继续由原调用方负责，本任务不顺带统一其已有差异。
- 相同 stat 且内容变化、mtime 又被恢复的文件仍可能无法被此快速指纹发现；本任务不把元数据指纹升级为内容完整性校验。
- 根路径、目录句柄、防越界、取消、缺失确认、快照切换、CAS 条件和后处理目标规则保持原语义。

## 实施增量与精确文件预算

以下都是 LUX-561 内的顺序增量，不同时开启其他 LUX 任务。每个增量先写能暴露错误的回归或编译期类型断言，运行最窄测试后原子提交。若实际影响超过预算，先调整拆分再修改。

| 增量 | 精确预计文件（均为仓库相对路径） | 验收与验证 |
|---|---|---|
| S0：本次设计 | `docs/LUX-DEVELOPMENT.md`、`docs/SHA256-FINGERPRINT-OPTIMIZATION.md` | 记录比较、证据、限制与完整实施计划；`git diff --check`。 |
| S1：固定数组计算与直接扫描 | `src/application/scanner.rs`、`src/application/scanner/library.rs`、`src/application/scanner/tests.rs`、`tests/scanner.rs` | 新增固定 golden vectors 和逐字段变化测试，先锁定旧字节；内部数组结果与旧包装一致。运行 scanner 库测试和 `--test scanner`。 |
| S2：准备所有权/SQL 边界 | `src/application/scanner/library/prepare.rs`、`src/application/scanner/jobs/manifest_discovery.rs`、`src/storage/jobs/scan/manifest/discovery/presence.rs` | 先让必要的 Vec 转换显式化，SQL 使用切片借用；仍保持当前 observation 类型可编译。运行 scanner 库测试、`--test scanning_jobs`。此准备增量单独不宣称提速。 |
| S3：固定 observation | `src/storage/repository.rs`、`src/application/scanner.rs`、`src/application/scanner/manifest.rs`、`src/application/scanner/tests.rs`、`src/storage/jobs/tests.rs` | 只迁移 `NewScanManifestEntry`，补齐历史 delta 长度处理和 fixture。运行 scanner/storage jobs 库测试、`--test scanning_jobs`；必要时 `--test storage`。 |
| S4：固定 seen-entry | `src/storage/repository.rs`、`src/application/scanner/jobs/manifest_discovery.rs`、`tests/scanning_jobs.rs` | 迁移 seen-entry，消除 S2 暂留的命中路径 Vec 转换。确认 unchanged generation CAS、并发改写、取消及 missing 回归通过。 |
| S5：实际性能与完成门 | `tests/performance.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md` | 记录真实扫描前后数据和未通过项，完成 Rust 检查；不能把内核微基准当成生产 CPU 数据。 |

S2/S3/S4 必须顺序执行以保证每次提交可编译。S3 若发现其他必须适配的所有权调用，先放进兼容当前 Vec 类型的 S2，不能留下中途编译失败的提交。

## 测试矩阵

1. 字节兼容：短路径、中文/emoji 路径、长路径、SHA block 边界、0/极值元数据、None/Some(0)。golden 期望值必须由当前基线生成并固定，不能用同一新 helper 给自己生成预期值。
2. 输入敏感：单独改变 path、size、纳秒 mtime、device、inode，必须保留旧判等结果；修改返回 Vec 不影响后续调用。
3. 主扫描：电影/剧集、local/STRM、媒体/旁车，首扫入库后重启重扫；稳定文件不得产生新增 probe、NFO 或后处理目标。
4. 并发安全：沿用 `unchanged_manifest_observation_uses_fingerprint_cas_before_advancing_generation` 等测试，确认并发改写不会被错误标记 seen；缺失、根路径替换和取消不得放行删除。
5. 存储边界：SQLite/PostgreSQL 借用相同 32 字节 BLOB；历史 NULL/异常长度锚点不改写；无 schema 变更。

最窄命令按增量选择，不以一个宽泛名称过滤替代集成测试 target：

```bash
# 每次执行 Cargo 前确认 Toshiba 已挂载，并避免并发使用共享 target。
mount | rg ' on /Volumes/Toshiba '
uname -m
export CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target

cargo test --locked --lib application::scanner::tests
cargo test --locked --lib storage::jobs::tests
cargo test --locked --test scanner
cargo test --locked --test scanning_jobs
cargo test --locked --test storage
```

完成时再运行 `cargo build --locked`、`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`。已有失败必须记录具体用例并在基线复现；不能自动勾选完成。无 Web 变更，不运行 Web 检查冒充验证；PostgreSQL 未配置时明确标为未验证。

## 性能验收与停止条件

- 用 `1e7c2ea8` 和实现分支的 release 构建交替运行至少 5 组，丢弃预热；两边固定相同数据库起点、fixture、并发与日志配置。
- 独立测摘要内核与 60k Manifest 首扫/无变化重扫，记录路径分布、摘要调用次数、总进程 CPU 时间、墙钟、分配、峰值 RSS、数据库调用数和扫描期间前台 p95。使用现有性能测试的临时库，不能连生产库。
- 分配验收聚焦新计算摘要及其 observation/seen 拷贝：这些位置不再产生 32 字节堆缓冲；数据库解码及必要业务 Vec 分配仍计入总量。可用 Instruments Allocations 等分配采样确认，不能仅凭返回类型声称整条扫描零分配。
- 60k 测试通过现有脚本分别选择旧扫描和 Manifest 目标，例如：

```bash
LUX_PERF_TEST_FILTER=lux_045_catalog_scan_benchmark ./scripts/run-performance.sh
LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark ./scripts/run-performance.sh
```

- 接受条件：旧字节逐项一致、后处理目标与 CAS 回归通过、目标临时分配消除、端到端没有可重复退化；微小时间差在噪声内时写“未观察到稳定端到端收益”。
- 如果出现中位数 >3% 的退化，先重跑确认；确认后不接受该增量。若热点采样显示摘要占比很低，不扩展到 A/C，不增加线程池、迁移或新依赖追逐小收益。
- 栈内数组仍可能因外层结构放在堆上；准确描述是“无独立摘要缓冲区分配”，不是“所有摘要永远在栈上”。

回滚是撤回相应代码增量；由于数据库字节和 schema 不变，无需数据迁移。推送、合并、部署与 NAS 性能验证不包含在本次设计交付中。

## 微基准复现附录

在独立临时目录保存下列 `src/main.rs`，Cargo.toml 使用 edition 2024、`sha2 = "=0.10.9"`；路径生成在计时外，`mode=0` 为 Vec，`mode=1` 为数组。完整结果为每种长度 22 行，首轮不进入统计。

```rust
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

#[inline(never)]
fn fixed(path: &str, n: i64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"LUX-FP-1\0");
    h.update((path.len() as u64).to_le_bytes());
    h.update(path.as_bytes());
    h.update(n.to_le_bytes());
    h.update(123456789123456789_i64.to_le_bytes());
    h.update(1_u64.to_le_bytes());
    h.update((n as u64).to_le_bytes());
    h.finalize().into()
}

#[inline(never)]
fn heap(path: &str, n: i64) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b"LUX-FP-1\0");
    h.update((path.len() as u64).to_le_bytes());
    h.update(path.as_bytes());
    h.update(n.to_le_bytes());
    h.update(123456789123456789_i64.to_le_bytes());
    h.update(1_u64.to_le_bytes());
    h.update((n as u64).to_le_bytes());
    h.finalize().to_vec()
}

fn main() {
    for len in [64_usize, 256, 512] {
        let paths: Vec<String> = (0..60000)
            .map(|i| format!("Movies/{i:06}/{}", "x".repeat(len - 14)))
            .collect();
        for round in 0..11 {
            for mode in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
                let start = Instant::now();
                let mut checksum = 0_u64;
                for (i, p) in paths.iter().enumerate() {
                    if mode == 0 {
                        let fp = heap(black_box(p), black_box(i as i64));
                        checksum += black_box(&fp)[0] as u64;
                    } else {
                        let fp = fixed(black_box(p), black_box(i as i64));
                        checksum += black_box(&fp)[0] as u64;
                    }
                }
                println!("{len},{round},{mode},{:.6},{checksum}",
                    start.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
}
```

```bash
# 在上述独立 crate 的目录运行，已确认 Toshiba 挂载。
export CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target
cargo generate-lockfile --offline
cargo run --release --locked --offline > raw.csv
```

两种函数使用相同的禁止内联边界。此隔离测试仍不包含生产调用点的内联、SQLx 编解码和并发分配器竞争，正式验收需在真实调用点及分配采样中复核。

参考资料：[sha2 0.10.9 API](https://docs.rs/sha2/0.10.9/sha2/)、[固定版本 SHA-256 后端选择源码](https://docs.rs/crate/sha2/0.10.9/source/src/sha256.rs)、[xxHash 官方格式与性能说明](https://github.com/Cyan4973/xxHash)。第三方吞吐量和硬件加速条件不能直接折算成 Lux 扫描收益；本地锁定版本的 hasher 初始化设置固定 IV，也不支持仅凭 `Sha256::new()` 就断言存在高昂的逐文件“rounds 设置”成本。
