# SQLite → PostgreSQL 离线迁移与一键 Compose

## 目标

让已有 SQLite 实例在停机窗口内安全迁移到 PostgreSQL，同时保留 SQLite 作为可直接回滚的事实副本。
迁移是管理员显式执行的运维动作，不进入 Lux HTTP 请求和正常启动路径。

## 不变量

- 源库始终以 SQLite `mode=ro` 打开，迁移工具不写 `/config`。
- PostgreSQL 密码仅从 `LUX_MIGRATE_POSTGRES_PASSWORD` 环境变量读取。
- 目标 schema 由当前 `migrations-postgres` 创建，且迁移前业务表必须为空。
- 全部业务复制、任务状态归一和搜索重建使用同一个目标事务。
- `_sqlx_migrations`、SQLite FTS5 影子表和可重建搜索数据不从源库复制。
- 成功前不创建或改写 `database.json`；切换由管理员在验证后显式完成。

## 实施切片

1. 规格与迁移清单
   - 建立稳定的业务表依赖顺序、排除清单与状态归一规则。
   - 单元测试先验证计划完整性、无重复表和敏感参数边界。
2. 离线迁移命令
   - 新增 `lux-db-migrate sqlite-to-postgres`。
   - SQLite 使用专用只读连接，PostgreSQL 先验证空库并运行迁移。
   - 每表分批读取，按 SQLite 动态值类型绑定到 PostgreSQL。
   - 事务内重建 `media_search`，逐表比对计数后提交。
3. 集成与运维
   - SQLite fixture 覆盖文本、BLOB、NULL、浮点及任务状态。
   - ignored PostgreSQL 集成测试覆盖真实迁移、登录、查询和搜索。
   - Compose 增加健康依赖和资源上限；文档给出快照、切换、验证和回滚步骤。

## 迁移顺序

父表先于子表：`lux_meta`、`users`、`libraries`、`library_roots`、`filesystem_entries`、
`media_items`、`media_sources`，随后复制会话、权限、图片、流、任务、候选、播放、合集、审计、
插件、弹幕和扫描工作队列表。`media_search` 最后从 `media_items` 与 `item_aliases` 重建。

## 验证

```bash
cargo test --locked --test database_migration
cargo test --locked --test postgres_database_migration -- --ignored
cargo build --locked --bin lux-db-migrate
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
docker compose config
```

真实 NAS 数据迁移需另行记录停机时间、源/目标大小、逐表计数、登录、媒体浏览、搜索和 Range 播放；
本机 ARM64 结果不代表飞牛 x86_64 性能。

2026-08-13：SQLite 计划/参数单元测试 6/6、迁移 CLI 测试 2/2、Compose 默认及
PostgreSQL profile 配置校验通过；在飞牛 x86_64 上的临时 PostgreSQL 17 容器完成真实往返测试，
覆盖源文件不变、逐表计数、BLOB、管理员登录、媒体查询、中文搜索别名、RUNNING 状态恢复及
非空目标拒绝。完整
`cargo test --all-targets` 仍被既有 LUX-169 插件目录版本断言（期望 4、实际 6）阻断；完整 Clippy
仅命中既有 `src/application/scanner.rs` 的 `nonminimal_bool`，均未在 LUX-173 中跨任务修改。
