# 阻塞文件 / 进程调用审计（分析项 C3）

本文档审计 `src/application/**`、`src/api/**`、`src/auth/**`、`src/observability/**` 中的
**非测试代码**，找出所有在 Tokio 核心 worker 上可能执行阻塞文件系统或阻塞进程工作的地方，
按调用位置归类，并记录修复与保留决策。

项目规则（`CLAUDE.md` / `AGENTS.md`）：**不在 Tokio 核心 worker 上执行阻塞的文件系统或进程工作。**

## 1. 审计范围与方法

- 范围：`src/application/**`、`src/api/**`、`src/auth/**`、`src/observability/**`。
- 检查对象：
  - `std::fs::*`（含 `use std::fs;` 后的 `fs::*`）；
  - `std::process::Command` 及同步子进程 API；
  - `std::thread::sleep`；
  - 同步 `File` / `OpenOptions` 读写（`File::open` / `File::create` / `File::options` /
    `OpenOptions::new`），以及 `std::io::Read`/`Write` 在其上的同步读写。
- 排除：`#[cfg(test)] mod tests`、`*_tests.rs`、`tests.rs` 之后的所有测试代码。
- 方法：
  1. 用脚本枚举所有候选调用点，并根据每个文件的 `use` 语句解析 `fs` 别名是 `std::fs` 还是
     `tokio::fs`（`use super::*` 形式的继承按父模块链解析）；带 `.await` 的调用判定为
     `tokio::fs`，不计入阻塞调用。
  2. 对每个 `std::fs` / 同步 `File` 调用点，沿调用链向上确认最近的 `spawn_blocking` /
     `run_process_background_blocking` / 专用线程 / 启动期入口，得到归类。
- 基线数字：全库 `spawn_blocking` 42 处、`run_process_background_blocking` 17 处（含导入），
  全部位于上述四个目录内。

## 2. 归类定义

| 标记 | 含义 |
| --- | --- |
| **a** | 调用点字面位于 `tokio::task::spawn_blocking` 或 `run_process_background_blocking` 闭包内 |
| **b** | 同步函数，且所有调用方都在 (a) / 专用线程 / 启动期 内 |
| **c** | 启动期单次初始化：构造 Runtime 之前，或在监听器绑定、首个请求之前的单次调用。标 **c\*** 的表示它虽然在 `#[tokio::main]` 的异步启动任务中执行，但只发生一次且无并发请求（属于启动期成本，不是热路径） |
| **d** | 在 async 函数（或 async 函数的同步调用链）里被直接调用，即违规 |

> `c*` 是对 (c) 的诚实标注：`src/main.rs` 里的启动阶段本身也是 async 的，因此这些调用技术上
> 仍在 Tokio worker 上执行，只是发生在服务对外可用之前。相关位置在后续建议里列出收敛方案。

## 3. 汇总

### 3.1 计数

| 项 | 数量 |
| --- | --- |
| 非测试代码中的阻塞文件系统调用点 | **124** |
| 归类 (a) | 13 |
| 归类 (b) | 86 |
| 归类 (c)（含 c\*） | 17 |
| 归类 (d) 违规 | **8** |
| 修复后剩余 (d) | **3**（均为非阻塞的 `File::try_lock`，见 §6.1） |
| 非测试代码中的 `std::process::Command` | **0** |
| 非测试代码中的 `std::thread::sleep` | **0** |

### 3.2 结论摘要

- 阻塞型**子进程**已经全部走 `tokio::process::Command`：全库 7 处 `Command::new`
  （`chapter_detector.rs:939`、`embedded_subtitle.rs:124`、`playback/hls.rs:967`、
  `plugin_runtime.rs:765`、`probe/runner.rs:103`、`thumbnails.rs:647`、`thumbnails.rs:725`）
  的导入来源均为 `tokio::process`，不构成阻塞调用；`std::process::Stdio` 只是句柄类型。
- 阻塞型**文件系统**工作绝大部分已经收敛到 `spawn_blocking` / 专用线程 / 启动期：
  扫描清单（`scanner/manifest.rs`，27 处）、日志存储（`observability/logs.rs`，37 处）、
  邮件配置（`mail.rs`，12 处）、插件发现（`plugin_runtime.rs`，12 处）都符合规则。
- 真正的违规 (d) 只有 8 处，集中在两类问题：
  1. **扫描遍历器的构造函数**用 `std::fs::canonicalize`（2 处）；
  2. **进程级惰性缓存**（`OnceLock`）在首次请求 / 首次扫描时才做文件系统读取（3 处）。
  3. **建议性文件锁**的 `File::try_lock()`（3 处，非阻塞 syscall，保留）。
- 另外 `plugin_runtime.rs` 的插件目录发现是**同步构造**（`PluginService::new`），目前只在启动期
  调用，属于 (c\*)；把它改成异步 + `spawn_blocking` 需要改动 `PluginService` 构造签名和十余处
  测试调用点，超出单点约 80 行的改动预算，列入后续建议。

## 4. 明细

### 4.1 `src/application/scanner/manifest.rs` — 27 处，全部 (b)

同步清单读取器（`ManifestDirectoryReader` 及 `*_sync` 函数）是刻意保持同步的：它用
`openat`/`fstatat`/`readdir` 句柄保证"打开目录时不跟随后续替换"的语义，无法用 `tokio::fs`
逐条替换。所有对外入口都是 async 包装，并把整块工作交给
`run_process_background_blocking(BackgroundWorkStage::ManifestPreparation, …)`
（本文件 733、756、776、1174、1196、1300）或 `spawn_blocking`
（`scanner/jobs/manifest_discovery.rs` 237、277、798、826）。

| 文件:行 | 调用 | 归类 | 处理 |
| --- | --- | --- | --- |
| scanner/manifest.rs:326 | `std::fs::File`（结构字段 `_directory`） | b | 保留：句柄由 `open_manifest_directory_identity` 在阻塞闭包内打开 |
| scanner/manifest.rs:368 | `Option<std::fs::File>` 返回类型 | b | 保留 |
| scanner/manifest.rs:369 | `std::fs::canonicalize` | b | 保留（`open_allowed_linked_directory`，仅被同步闭包调用） |
| scanner/manifest.rs:370 | `std::fs::canonicalize` | b | 保留 |
| scanner/manifest.rs:379 | `std::fs::OpenOptions::new` | b | 保留 |
| scanner/manifest.rs:390 | 返回 `(std::fs::File, …)` | b | 保留 |
| scanner/manifest.rs:397 | `std::fs::OpenOptions::new` | b | 保留 |
| scanner/manifest.rs:530 | `std::fs::canonicalize` | b | 保留（`ManifestDirectoryReader::open_with_policy`） |
| scanner/manifest.rs:960 | `std::fs::symlink_metadata` | b | 保留（`stat_manifest_relative_file_sync`） |
| scanner/manifest.rs:1009 | `std::fs::canonicalize` | b | 保留（`followed_symlink_observation`） |
| scanner/manifest.rs:1028 | `std::fs::metadata` | b | 保留 |
| scanner/manifest.rs:1226 | `std::fs::canonicalize` | b | 保留（`verify_manifest_directory_observation_sync`） |
| scanner/manifest.rs:1231 | `std::fs::symlink_metadata` | b | 保留 |
| scanner/manifest.rs:1250 | `std::fs::canonicalize` | b | 保留 |
| scanner/manifest.rs:1255 | `std::fs::symlink_metadata` | b | 保留 |
| scanner/manifest.rs:1266 | `std::fs::metadata` | b | 保留 |
| scanner/manifest.rs:1356 | `std::fs::canonicalize` | b | 保留（`stat_manifest_directory_file_batch_sync`） |
| scanner/manifest.rs:1419 | `&std::fs::File` 参数 | b | 保留 |
| scanner/manifest.rs:1567 | `std::fs::File::from_raw_fd` | b | 保留（`read_manifest_strm_target_sync`） |
| scanner/manifest.rs:1915 | 返回 `std::fs::File` | b | 保留（`open_manifest_directory_component`） |
| scanner/manifest.rs:1939 | `std::fs::File::from_raw_fd` | b | 保留 |
| scanner/manifest.rs:1952 | `std::fs::ReadDir`（结构字段） | b | 保留 |
| scanner/manifest.rs:1981 | `std::fs::canonicalize` | b | 保留（非 Unix 回退实现） |
| scanner/manifest.rs:1991 | `std::fs::metadata` | b | 保留 |
| scanner/manifest.rs:2003 | `std::fs::canonicalize` | b | 保留 |
| scanner/manifest.rs:2015 | `std::fs::metadata` | b | 保留 |
| scanner/manifest.rs:2026 | `std::fs::read_dir` | b | 保留 |

### 4.2 `src/observability/logs.rs` — 37 处，全部 (b)

日志存储刻意使用**同步**文件 IO：它既被 JSON 日志 writer 使用，也被审计 / 任务日志的读取接口
使用。writer 侧由 `tracing_appender::non_blocking`（`observability/mod.rs:46`
`thread_name("lux-log-writer")`）投递到**专用线程**；读取侧全部包在
`tokio::task::spawn_blocking`（本文件 129、148、173、208、225、273、350、399、414、1433、1459）。

| 文件:行 | 调用 | 归类 | 处理 |
| --- | --- | --- | --- |
| logs.rs:500、525 | `fs::read_dir` | b | 保留（`read_scan_job_events`，由 spawn_blocking 350 调用） |
| logs.rs:514、541 | `fs::File::open` | b | 保留 |
| logs.rs:600、626 | `fs::read_dir` | b | 保留（`read_record_ids`，由 spawn_blocking 273 调用） |
| logs.rs:615、642 | `fs::File::open` | b | 保留 |
| logs.rs:749、771 | `fs::read_dir` | b | 保留（`read_audit_events`，由 spawn_blocking 399 调用） |
| logs.rs:764、787 | `fs::File::open` | b | 保留 |
| logs.rs:862、885 | `fs::read_dir` | b | 保留（`read_activity_events`，由 spawn_blocking 414 调用） |
| logs.rs:877、901 | `fs::File::open` | b | 保留 |
| logs.rs:1025 | `fs::create_dir_all` | b | 保留（`LogManager::open` 仅在阻塞上下文 / 专用线程内调用） |
| logs.rs:1029、1032 | `fs::metadata` / `fs::set_permissions` | b | 保留（`create_private_directory`） |
| logs.rs:1043、1046 | `fs::metadata` / `fs::set_permissions` | b | 保留（`restrict_log_file_permissions`） |
| logs.rs:1096 | `fs::read_dir` | b | 保留（`LogManager` 启动扫描） |
| logs.rs:1113 | `fs::OpenOptions::new` | b | 保留（打开活动日志文件） |
| logs.rs:1205、1212、1222、1234 | `fs::OpenOptions::new` / `fs::remove_file` / `fs::File::open` | b | 保留（日志分段归档） |
| logs.rs:1246、1247、1254 | `fs::rename` / `fs::remove_file` | b | 保留（归档原子替换） |
| logs.rs:1262、1279、1292 | `fs::read_dir` / `fs::remove_file` | b | 保留（归档裁剪） |
| logs.rs:1480、1493、1529、1604 | `fs::read_dir` / `fs::File::open` / `fs::read` | b | 保留（`read_log_files` / `create_archive` / `verify_archive`，由 spawn_blocking 1433、1459 调用） |

说明：`logs.rs:1015`（`active_file: Option<fs::File>`）与 `logs.rs:1298`
（`fn repair_incomplete_jsonl_tail(file: &fs::File)`）是**类型引用**，不计入调用点。

### 4.3 `src/application/mail.rs` — 12 处，全部 (b)

`read_private_mail_challenge_signing_key` / `create_private_file_if_absent` /
`write_private_file_atomically` 是同步函数（需要 `create_new` + `fsync` + `hard_link`/`rename`
的原子语义，`tokio::fs` 无法提供多步原子保证），其唯一调用方都在
`tokio::task::spawn_blocking`（本文件 148、323、353）内。

| 文件:行 | 调用 | 归类 | 处理 |
| --- | --- | --- | --- |
| mail.rs:378 | `std::fs::symlink_metadata` | b | 保留 |
| mail.rs:395 | `std::fs::read` | b | 保留 |
| mail.rs:416 | `std::fs::OpenOptions::new` | b | 保留 |
| mail.rs:427 | `std::fs::Permissions::from_mode` | b | 保留 |
| mail.rs:432 | `std::fs::hard_link` | b | 保留 |
| mail.rs:435 | `std::fs::File::open(...).sync_all()` | b | 保留（父目录 fsync） |
| mail.rs:442 | `std::fs::remove_file` | b | 保留 |
| mail.rs:458 | `std::fs::OpenOptions::new` | b | 保留 |
| mail.rs:469 | `std::fs::Permissions::from_mode` | b | 保留 |
| mail.rs:473 | `std::fs::rename` | b | 保留 |
| mail.rs:475 | `std::fs::File::open(...).sync_all()` | b | 保留 |
| mail.rs:481 | `std::fs::remove_file` | b | 保留 |

（`mail.rs` 其余 `fs::*` 调用均为 `tokio::fs`，带 `.await`，合规。）

### 4.4 `src/application/plugin_runtime.rs` — 12 处，全部 (c\*)

插件目录发现（`PluginCatalog::discover` → `discover_directory` / `discover_archive` /
`read_manifest` / `verify_declared_files` / `extract_archive` / `sha256_file`）是同步实现。
唯一"非阻塞包装"的入口是 `plugins.rs:2720 discover_plugin_catalog`（`spawn_blocking`）；
冷启动路径 `PluginService::build_with_proxy`（`plugins.rs:198`）直接同步调用，由
`AppState::ready_with_proxy_and_image_config`（`src/api/legacy.rs:308`）在启动期执行，
此时监听器尚未绑定。

| 文件:行 | 调用 | 归类 | 处理 |
| --- | --- | --- | --- |
| plugin_runtime.rs:76 | `fs::read_dir` | c\* | 保留 + 后续建议（见 §7.1） |
| plugin_runtime.rs:904 | `fs::metadata` | c\* | 保留（`discover_archive`） |
| plugin_runtime.rs:910 | `File::open` | c\* | 保留 |
| plugin_runtime.rs:942 | `fs::metadata` | c\* | 保留（`read_manifest`） |
| plugin_runtime.rs:954 | `fs::read_to_string` | c\* | 保留 |
| plugin_runtime.rs:1027 | `fs::remove_dir_all` | c\* | 保留（`extract_archive`） |
| plugin_runtime.rs:1029 | `fs::create_dir_all` | c\* | 保留 |
| plugin_runtime.rs:1040 | `fs::create_dir_all` | c\* | 保留 |
| plugin_runtime.rs:1044 | `fs::create_dir_all` | c\* | 保留 |
| plugin_runtime.rs:1046 | `File::create` | c\* | 保留 |
| plugin_runtime.rs:1066 | `fs::set_permissions` | c\* | 保留 |
| plugin_runtime.rs:1073 | `File::open` | c\* | 保留（`sha256_file`） |

热路径（安装、刷新目录、校验上传包）已全部经过 `discover_plugin_catalog` 的
`spawn_blocking`（`plugins.rs:720、2448、2478`）。

### 4.5 其余 `(a)` 类（字面在 `spawn_blocking` 闭包内）— 13 处

| 文件:行 | 调用 | 归类 | 处理 |
| --- | --- | --- | --- |
| application/plugin_store.rs:167 | `std::fs::OpenOptions::new` | a | 合规 |
| application/plugin_store.rs:174 | `std::fs::rename` | a | 合规 |
| application/plugin_store.rs:276 | `std::fs::OpenOptions::new` | a | 合规 |
| application/webhooks.rs:1702 | `OpenOptions::new` | a | 合规 |
| application/webhooks.rs:1708 | `std::fs::set_permissions` | a | 合规 |
| application/webhooks.rs:1714 | `std::fs::rename` | a | 合规 |
| auth/admin_api_key.rs:102 | `OpenOptions::new` | a | 合规 |
| auth/admin_api_key.rs:108 | `std::fs::set_permissions` | a | 合规 |
| auth/admin_api_key.rs:114 | `std::fs::rename` | a | 合规 |
| application/settings.rs:1124 | `std::fs::OpenOptions::new` | a | 合规（`write_secret_file`） |
| application/settings.rs:1130 | `std::fs::set_permissions` | a | 合规 |
| application/playback/hls.rs:1594 | `std::fs::read_dir` | a | 合规（`latest_segment_bitrate_kbps`，整次遍历一次性 hop） |
| application/playback/hls.rs:1782 | `std::fs::read_dir` | a | 合规（`session_directory_bytes` 配额统计） |

### 4.6 其余 `(b)` 类 — 10 处

（`(b)` 合计 86 处 = 4.1 的 27 + 4.2 的 37 + 4.3 的 12 + 本表 10。）

| 文件:行 | 调用 | 归类 | 处理 |
| --- | --- | --- | --- |
| application/image_variants.rs:627 | `std::fs::metadata` | b | 合规（`generate_variant_blocking`，唯一调用方 `spawn_blocking` @354） |
| application/image_variants.rs:631 | `std::fs::read` | b | 合规 |
| application/image_variants.rs:670 | `std::fs::create_dir_all` | b | 合规 |
| application/image_variants.rs:673 | `std::fs::write` | b | 合规 |
| application/image_variants.rs:674 | `std::fs::rename` | b | 合规 |
| application/image_variants.rs:677 | `std::fs::remove_file` | b | 合规 |
| application/scan_fingerprint_migration.rs:344 | `std::fs::canonicalize` | b | 合规（`observe_candidate`，调用方 `spawn_blocking` @277、294） |
| application/scan_fingerprint_migration.rs:358 | `std::fs::canonicalize` | b | 合规 |
| application/scan_fingerprint_migration.rs:362 | `std::fs::metadata` | b | 合规 |
| application/watch.rs:525 | `std::fs::canonicalize` | b | 合规（`normalize_root` ← `LibraryWatcher::new`，只在 `lux-watcher-init` 专用线程执行） |

### 4.7 `(c)` / `(c*)` 其余 — 5 处

| 文件:行 | 调用 | 归类 | 处理 |
| --- | --- | --- | --- |
| application/container_gateway.rs:50 | `std::fs::read_to_string`（`/proc/self/cgroup`） | c\* | 合规：`main.rs:26` 显式 `warm_up()` 预热 `OnceLock` |
| application/container_gateway.rs:51 | `std::fs::read_to_string`（`/proc/net/route`） | c\* | 同上 |
| application/container_gateway.rs:52 | `std::fs::read_to_string`（`/proc/net/ipv6_route`） | c\* | 同上 |
| application/settings.rs:1054 | `std::fs::read_to_string`（`plugin-proxy-bypass`） | c\* | 合规：唯一调用方 `plugins.rs:201`（启动期构造） |
| application/settings.rs:1086 | `std::fs::read_to_string`（`read_secret`） | c\* | 合规：唯一调用方 `main.rs:187`（启动期） |

## 5. `(d)` 违规与修复

| # | 文件:行 | 调用 | 问题 | 修复 |
| --- | --- | --- | --- | --- |
| 1 | application/scanner/library.rs:294 | `std::fs::canonicalize` | `FileBatchWalker::new` 在 async 扫描循环中同步规范化根路径 | 改为 `async fn` + `tokio::fs::canonicalize`；5 处调用点加 `.await` |
| 2 | application/scanner/library.rs:303 | `std::fs::canonicalize` | `FileBatchWalker::new_with_sidecars` 同上 | 同上 |
| 3 | application/transcoding.rs:34 | `std::fs::read_dir("/dev/dri")` | `detected_hardware_encoders()` 的 `OnceLock` 首次填充发生在管理员设置请求里 | 新增 `warm_up()`，在启动期 `AppState::ready_with_proxy_and_image_config` 预热 |
| 4 | application/transcoding.rs:45 | `std::fs::read_to_string`（sysfs vendor） | 同上 | 同上 |
| 5 | application/scanner/symlink.rs:145 | `std::fs::canonicalize` | `SymlinkPolicy::current()` 的 `OnceLock` 首次填充可能发生在扫描 / 请求路径 | 新增 `warm_up()`，在启动期预热 |
| 6 | application/people/helpers.rs:1269 | `std::fs::File` 句柄字段 | 句柄来自 `tokio::fs::OpenOptions::…open().into_std().await`，打开本身是异步的 | 保留（见 §6.1） |
| 7 | application/people/helpers.rs:1296 | `File::try_lock()` → `WouldBlock` | 非阻塞 `flock(LOCK_NB)`，立即返回 | 保留（见 §6.1） |
| 8 | application/people/helpers.rs:1304 | `File::try_lock()` → `Error` | 同上 | 保留（见 §6.1） |

修复后 (d) 剩余 **3** 处（均为 §6.1 中记录为可接受的非阻塞调用）。

## 6. 保留的 (d) 类及理由

### 6.1 `people/helpers.rs` 的建议性文件锁

`acquire_exclusive_file_lock`（`people/helpers.rs:1271`）用
`tokio::fs::OpenOptions::…open(...).await` 打开锁文件，再 `into_std().await` 得到
`std::fs::File`，循环里调用 `file.try_lock()`，重试间隔用 `tokio::time::sleep`。

- `try_lock()` 对应 `flock(LOCK_NB)`，**不会挂起线程**，只是进内核取一次锁状态；
- 真正可能阻塞的 `open` 已经是 `tokio::fs`（内部 `spawn_blocking`）；
- 用 `spawn_blocking` 包住 `try_lock` 反而会把一次微秒级 syscall 变成一次线程调度，
  并破坏"锁句柄随 guard 存活"的所有权模型。

因此保留，并在本节记录为**可接受的非阻塞调用**，不计入待修复量。

## 7. 后续建议（本次未做，超出单点 ~80 行预算）

1. **`PluginService::new` 异步化**：把 `PluginCatalog::discover`（`plugin_runtime.rs`，12 处同步
   `std::fs`，含 zip 解包与逐文件 SHA-256）搬到 `spawn_blocking`。现状只在启动期执行（合规于
   (c\*)，但启动期耗时可能达秒级）。需要把 `PluginService::new` / `new_with_proxy` 改成
   `async fn`，并同步修改 `src/api/legacy.rs` 与十余处测试构造点，改动约 200+ 行。
2. **启动期阻塞收敛**：把 `main.rs` 的 `build_app_state` 中对 `AppState::ready_*` 的调用整体
   包进 `spawn_blocking`（或把 `ready_*` 拆成"纯数据"与"文件系统初始化"两段）。涉及
   `src/main.rs`（本次禁改）与 `src/api/legacy.rs` 的构造顺序，建议单独任务处理。
3. **启动期预热统一入口**：目前 `container_gateway::warm_up()` 在 `main.rs`、本次新增的
   `transcoding::warm_up()` 与 `scanner::symlink::warm_up()` 在 `api/legacy.rs`，位置分散；
   建议后续统一为一个 `startup::warm_up_caches()`。
4. **为 (d) 类加静态检查**：可以在 CI 里拒绝 `src/application|api|auth|observability` 下
   `#[cfg(not(test))]` 区域内新增的 `std::fs::` / `std::process::Command` / `std::thread::sleep`，
   要求显式 `#[allow]` 或白名单注释，避免回退。
