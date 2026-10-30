# Database 按领域拆分：notifications 域试点（分析项 B1）

状态：试点完成，2026-10-30。分支 `tspace/database-split-notifications`，基线 `test@5c1b1a6b`（合并 test 时为 fast-forward，无冲突）。
范围只有 notifications 一个域，行为、SQL 与方法签名（除接收者外）不变。

结论先行：**"访问器 + 薄仓库对象"模式可用，且对 notifications 这种"外部调用点少、不跨事务"的域成本很低；但它不带来编译时间收益，也不自动带来类型封装，不建议对全部 638 个方法做一次性迁移。建议仅对新代码采用，并按域灰度迁移。**

## 1. 本次改了什么

- 新增 `pub(crate) struct NotificationsRepo<'a> { db: &'a Database }`，位于 `src/storage/notifications.rs`；同一文件里给出访问器 `impl Database { pub(crate) fn notifications(&self) -> NotificationsRepo<'_> }`。
- `src/storage/notifications.rs` 的 14 个方法从 `impl Database` 迁到 `impl NotificationsRepo<'_>`，方法体只把接收者从 `self` 换成 `self.db`：

  `create_notification_destination`、`find_notification_destination`、`list_notification_destinations`、
  `list_enabled_notification_destinations`、`update_notification_destination`、`delete_notification_destination`、
  `insert_notification_event_with_deliveries`、`list_ready_notification_deliveries`、`prune_notification_history`、
  `list_notification_deliveries`、`claim_notification_delivery`、`mark_notification_delivered`、
  `mark_notification_retry`、`retry_notification_delivery`

- 文件头从 `use super::*;` 改成显式导入列表（11 个符号 + `uuid::Uuid`）。
- **没有**放宽任何 helper 的可见性：`Database::query`、`Database::sqlx_err` 和私有字段 `pool` 仍保持原可见性即可用，因为 `notifications` 是 `repository` 的子模块（见 6.3）。
- `src/storage/repository.rs` 的**生产代码零改动**；`src/storage/mod.rs` 未改（原因见 6.4）。
- 32 个调用点改为 `database.notifications().<method>(...)`。

## 2. 调用点改动量（实测）

| 文件 | 调用点 | 行数变化（+ / −） | 性质 |
|---|---:|---|---|
| `src/application/webhooks.rs` | 17 | +36 / −4 | 生产代码 |
| `src/storage/repository_tests.rs` | 12 | +15 / −1 | `#[cfg(test)]` 库测试 |
| `src/storage/repository.rs`（内联 `stored_row_decoding_tests`） | 3 | +6 / −1 | `#[cfg(test)]` 库测试 |
| `src/storage/notifications.rs` | — | +157 / −118 | 迁移本体（`git diff -w` 后为 +90 / −51，其余约 67 行是 rustfmt 重排） |
| `src/storage/mod.rs` | 0 | 未改 | 无需改动 |
| `tests/**` | **0** | 未改 | 全库 `.rs` 扫描确认无调用，因此不需要兼容转发，也没有添加 |

合计 4 个文件、+214 / −124 行；其中真正语义变化的是 14 个方法定义与 32 个调用点的接收者。32 处里 26 处只是插入一个 `.notifications()`，6 处因超过行宽被 rustfmt 拆成多行链。

耗时记录（墙面时间，本机）：

- 扫描 + 阅读现状：约 6 分钟（用脚本枚举 14 个方法名在全库的出现位置，得到 32 个调用点，`tests/` 为 0）。
- 迁移方法体 + 改写调用点 + 修正 3 处跨行 `self` 接收者 + 去掉多余再导出：约 11 分钟，全部为脚本辅助 + 逐处人工确认。
- 等编译/验收：约 12 分钟（含一次 2m34s 冷编译与一次 2m03s 测试目标编译）。
- **每调用点成本约 20 秒（含方法迁移摊销）**；每方法约 45 秒。

## 3. 编译影响（arm64 / macOS / 10 核）

| 场景（`--locked`，debug） | 改动前 | 改动后 |
|---|---|---|
| 空 `target/` 全量 `cargo build` | 2m34s | 未重测（改动不涉及依赖） |
| 仅 `touch src/storage/notifications.rs` 后 `cargo build` | 23.5s / 68.6s / 29.9s | 18.3s / 26.9s |
| `cargo test --lib --no-run` | 2m03s | 1m48.8s |
| 整块改动后 `cargo build`（首次重编） | — | 37.7s / 41.9s / 55.7s |
| `cargo clippy --all-targets --all-features` | 未测 | 4m09s |

**不要把上表读成收益或退化。** 采样期间机器上同时有另外两个 worktree 在编译/测试（`fromrow-repo-2`、`fromrow-catalog-1`），load average 在 5.8–8.4 之间波动，单个数字的抖动（改动前 23.5s→68.6s）比改动前后的差异大一个量级；改动后的最大值反而出现在机器最忙时。

结构性原因：本仓库是单体 crate，`src/storage/*.rs` 只是模块文件，改任何一个文件都重编整个 `luxd` crate。**按域拆分不会改变编译单元，因此不应期待编译时间收益**；改动前后唯一的编译差异是增量缓存的命中内容，属于噪声。若将来把域拆成独立 crate（本次未做，也超出任务范围），编译收益才有意义——但那样 `NotificationsRepo` 就必须通过 `pub(crate)`/`pub` 的 helper 访问 `Database`，会引入本次刻意避免的可见性放宽。

## 4. 可见性收益

立即获得的收益：

1. `Database` 的表面少了 14 个方法。通知操作只能经 `database.notifications()` 到达，reviewer 一眼能看出"这行在写通知表"。
2. `NotificationsRepo` 在 `storage` 之外**不可命名**（见 6.4），调用点拿到的是一个只能调方法的值，无法把它存进字段、扩展 trait 或当作 `Database` 的替身继续使用。
3. `notifications.rs` 不再 `use super::*;`。原先该文件可以无成本引用 `repository` 里的任何私有符号（例如别的域的 helper、别的域的 `Stored*Row`），现在依赖面变成 11 个显式符号，新增依赖必须显式声明——这是本试点唯一"立刻可验证收紧"的一项，代价只是 8 行 `use`。

本次**没有**收紧、但已经具备条件、建议后续处理的项（用 `grep` 确认仅本域使用）：

| 符号（当前在 `repository.rs`） | 现状 | 可收紧为 |
|---|---|---|
| `StoredNotificationDestinationRow` | 私有 struct，仅 notifications.rs 使用 | 移入 `notifications.rs`，模块私有 |
| `fn stored_notification_destination` | 私有 fn，仅本域使用 | 同上 |
| `StoredNotificationDeliveryRow` | 私有 struct，仅本域使用 | 同上 |
| `fn stored_notification_delivery` | 私有 fn，仅本域使用 | 同上 |

没有做的原因：这会把"纯接收者替换"变成"移动类型定义"，需要与并行修改 `repository.rs` 的任务协调，超出本次试点范围。

没有获得的收益（重要）：5 个 DTO（`NewNotificationDestination`、`UpdateNotificationDestination`、`StoredNotificationDestination`、`NewNotificationEvent`、`StoredNotificationDelivery`）仍在 `repository.rs`，并且仍经 `storage/mod.rs` 对全 crate 导出。也就是说：**方法收进去了，数据形状没进去**，域封装只完成一半；任何调用方仍可直接构造 `NewNotificationDestination`。若要完整封装，DTO 也要随域迁移并改成 `pub(in crate::storage)`，这会给 `src/application` 的构造点带来连锁改动（本试点 17 处 webhooks 调用都会受影响）。

## 5. 推广判断

### 工作量估算

- 调用点：`src/application` + `src/api` + `src/auth` 中 `database` 相关行实测 2499 行（与任务给出的"约 2800 处"同数量级，我的统计偏保守，没有逐处区分是否真正的调用）。按本次 **20 秒/调用点** 计，机械部分约 **14 小时 ≈ 2 人日**。
- 方法迁移：638 方法 ÷ 14 ≈ 46 个批次，每批次含一次全量构建 + 相关测试（约 10–15 分钟），约 **8–12 小时**；叠加逐域肉眼复核与借用问题处理，整体 **3–5 人日**。
- 冲突成本（往往被低估）：`repository.rs` 是并行任务的公共热点，本次试点证明把访问器放在域文件里可以让 `repository.rs` 生产代码零改动，这一点降低了冲突面；但每个域的私有 helper 收紧仍要动 `repository.rs`。

### 风险

1. 借用生命周期：`db.notifications()` 借用 `&Database`，仓库对象不能跨语句保存，也不能与 `Database` 的 `&mut self` 方法共存（当前 `Database` 没有 `&mut self` 方法，所以暂无实际冲突）。
2. 长生命周期/后台任务：`Database` 是 `#[derive(Clone)]`，任务里普遍 move 一份 `Database`。迁到仓库对象后，必须在任务内部调用访问器；这一点在本域不构成问题，但在 `jobs`/扫描这类持有 `db` 的域会增加改造面。
3. 事务边界：仓库对象只借用 `&Database`，无法表达"在调用方事务内执行"。本域唯一的多语句事务在方法内部自建，因此没问题；`metadata`/`media`/`catalog` 等大量依赖 `begin_*_transaction` 与 `&mut *tx` 的域需要先逐方法确认，风险显著更高。
4. 收尾不彻底：如 4 节所述，只迁方法不迁 DTO/行结构，收益主要是"调用点显式化"，不是真正的模块边界。

### 建议

**仅对新代码采用 + 按域灰度迁移，不做一次性全量迁移。** 理由：

- 编译收益为 0（单 crate），性能收益为 0（同一 pool、同一 SQL），收益主要在可读性与 review 成本。
- 成本与风险都不均匀：通知域 14 个方法、32 个调用点、0 处可见性改动、0 处借用摩擦；换成 catalog/media 这类千行级、跨事务、被 `application` 与 `api` 大量引用的域，每域的摩擦点数量级不同。
- 优先级排序建议：先做**外部调用点 < 50、且不依赖跨方法事务**的域；对于体量大的域，先只做"文件级拆分"（现状已如此：一个域一个文件、仍 `impl Database`）——这已经是零成本的可读性收益，迁移成仓库对象的边际收益不足以支撑 2800 处调用点的改动。
- 如果项目目标转向"域可独立成 crate"或"禁止跨域直接访问表"，那么这次试点是必要的第一块砖，但需要把 DTO 与行结构一并纳入，并接受 `query`/`sqlx_err`/`pool` 的可见性放宽。

## 6. 试点中发现的问题（逐条）

### 6.1 借用生命周期

`db.notifications().mark_notification_delivered(...).await` 依赖语句级临时值，`NotificationsRepo` 存活到语句结束，临时值借用检查通过。但：

- 不能 `let repo = db.notifications();` 后跨语句使用，除非 `db` 在此期间不被借用（这里 `db` 都是 `&Database`，可行，但会把 `&Database` 的借用期拉长，若调用方随后要 move `db`（例如 `tokio::spawn`）就会冲突）。
- 返回引用 `self.db` 的方法签名无需改动（`&self` 仍是 `&self`），`self.db` 是 `Copy` 的共享引用，因此 async fn 的 future 不会额外捕获更多东西——这是该模式对 async 友好的关键，本次没有出现任何生命周期标注负担。

### 6.2 事务内无法使用

`NotificationsRepo` 只能拿到 `&Database`（`pool`、`query`），无法拿到调用方的事务句柄。本域没有"在调用方事务内执行"的方法，所以未受影响；但若某域同时有"独立事务"和"参与调用方事务"两种变体，仓库对象需要额外接受 `&mut Transaction` 参数（或改造为 trait 泛型 executor），成本会立刻上升。

### 6.3 `#[path]` 模块结构：本次"零可见性放宽"的原因与限制

`repository.rs` 用 `#[path = "notifications.rs"] mod notifications;` 把文件挂成 `repository` 的**子模块**。Rust 的私有项对子模块可见，所以 `self.db.query(...)`、`self.db.sqlx_err()`、`self.db.pool` 全部照旧可用，**不需要任务预设的 `pub(super)` 放宽**。

限制：这一便利与当前目录布局强绑定。一旦想把域做成 `src/storage/notifications/{mod.rs,repo.rs}` 这类多文件结构，`query`/`sqlx_err`/`pool` 就必须放宽到 `pub(super)`/`pub(crate)`，`Database` 的内部实现细节就会对 `storage` 全域可见（目前只有 `sqlx_err`/`storage_error` 是 `pub(super)`）。换句话说：**现在看起来很干净的"0 处可见性改动"，是模块树位置的产物，不是模式本身的属性**。

### 6.4 再导出与类型可见性

计划中允许修改 `src/storage/mod.rs` 做导出，实测**不需要**：

- 若要把 `NotificationsRepo` 再导出，需要在 `repository.rs` 内写 `pub(crate) use notifications::NotificationsRepo;`（`notifications` 模块对 `repository` 私有，不能从 `mod.rs` 经 `repository::` 路径直接再导出）；再导出后 `cargo build` 会报 `unused import`（`-D warnings` 下失败），因为所有调用点只是对返回值调用固有方法，从不命名该类型。因此本试点没有再导出，也没有改 `mod.rs`。
- 因此 `src/storage/mod.rs` 保持未改，`NotificationsRepo` 在 `storage` 外不可命名。编译期也没有触发 `private_interfaces` 告警：在 `pub(crate) fn notifications()` 返回该类型且不导出时，`cargo build` 与 `cargo clippy ... -D warnings` 均无告警通过。
- 副作用：如果将来要把它存进 struct 字段、作为返回值、或写进 `pub(crate)` 的 trait 对象，就必须导出，届时需要重新确认可见性策略。

### 6.5 rustfmt 噪声

`self.` → `self.db.` 让若干 `.bind(...).execute(&self.db.pool)` 链超出宽度阈值，rustfmt 把它拆成多行，产生约 67 行纯格式改动（`notifications.rs` 的 +157/−118 中占比不小：`git diff -w` 后只有 +90/−51）。

实测替代方案：在方法体首行写 `let db = self.db;`，后续用 `db.query(...)`/`&db.pool`/`db.sqlx_err()`，同一文件的 diff 可降到 **+86 / −54**（用同一份基线文件生成别名变体并 `rustfmt` 后对比，未采用）。没有采用的原因：638 个方法的规模下，隐式别名容易忘写或误用；且它掩盖了"这里借的是 `Database`"这一事实。若规模化迁移时希望 review diff 更小，这是一个已验证的降噪选项。

### 6.6 测试与并行任务

- 调用点里有 15 处在 `#[cfg(test)]` 代码（`repository_tests.rs` 12 + `repository.rs` 内联 3），属于 `src/`，正常改动即可。
- `tests/**` 全库扫描无调用，因此**不需要**（也不允许）留 `#[doc(hidden)]` 转发；本次没有引入任何兼容层。
- 本次未触碰 `catalog.rs`、`media.rs` 等并行任务文件；`repository.rs` 的生产代码也没有变化，只改了其中内联测试模块的 3 行调用。

## 7. 验收（本机 arm64，`cargo` = `~/.cargo/bin/cargo`）

| 检查 | 结果 |
|---|---|
| `cargo build --locked` | 通过 |
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | 通过（4m09s，无告警） |
| `python3 scripts/check-backend-branches.py` | 通过（28 files, 266 branches） |
| `cargo test --locked --lib storage::` | 通过：451 passed, 0 failed, 37 ignored（用例耗时 422.67s，墙体 9m46s） |
| `cargo test --locked --lib notification` | 通过：15 passed, 0 failed, 1 ignored（PostgreSQL 用例需要本地实例） |

复现（枚举调用点用的脚本，改写前先跑一遍确认范围）：

```bash
python3 - <<'EOF'
import re, pathlib
methods = ["create_notification_destination", "find_notification_destination",
  "list_notification_destinations", "list_enabled_notification_destinations",
  "update_notification_destination", "delete_notification_destination",
  "insert_notification_event_with_deliveries", "list_ready_notification_deliveries",
  "prune_notification_history", "list_notification_deliveries",
  "claim_notification_delivery", "mark_notification_delivered",
  "mark_notification_retry", "retry_notification_delivery"]
pattern = re.compile(r"\.(" + "|".join(methods) + r")\s*\(")
for path in sorted(pathlib.Path(".").rglob("*.rs")):
    if "target/" in str(path):
        continue
    for number, line in enumerate(path.read_text(errors="replace").splitlines(), 1):
        if pattern.search(line):
            print(f"{path}:{number}: {line.strip()}")
EOF

cargo build --locked
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
python3 scripts/check-backend-branches.py
cargo test --locked --lib storage::
cargo test --locked --lib notification
```

## 8. 后续可执行项

1. 把 `StoredNotificationDestinationRow`/`StoredNotificationDeliveryRow` 与两个 `stored_notification_*` 转换函数移入 `notifications.rs`（已确认仅本域使用），完成该域的可见性收紧。
2. 若继续推广：给 5 个通知 DTO 一并迁入做好准备（会牵动 `src/application/webhooks.rs` 的构造点）。
3. 若决定推广：在 `docs/LUX-DEVELOPMENT.md` 中把"新存储代码优先写成域仓库对象"作为约定落地（本次未改该文件），再按 5 节的排序逐域灰度。
