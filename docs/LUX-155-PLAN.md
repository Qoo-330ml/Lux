# LUX-155：首次引导选择 SQLite 或 PostgreSQL

## 范围

首次初始化、创建第一个管理员之前，用户可以选择内置 SQLite 或外部 PostgreSQL。SQLite 保持默认行为；PostgreSQL 由用户在 Lux 之外运行，Lux 负责连接测试、迁移和业务读写。

本任务不支持已初始化实例在线切换数据库，不自动迁移已有 SQLite 数据，不支持 MySQL，不在 Lux 容器内启动 PostgreSQL 子进程。

## 契约

- 未配置数据库时，Lux 仍启动 HTTP 服务，但只提供静态资源、健康检查和数据库引导端点。
- `GET /api/v1/setup/database` 返回当前数据库配置状态和可选后端，不返回密码或完整 DSN。
- `POST /api/v1/setup/database/test` 接受 `SQLITE` 或 `POSTGRESQL` 配置，验证输入和连接；失败返回结构化、脱敏错误。
- `POST /api/v1/setup/database/select` 原子保存后端配置、运行对应 migration，并使业务 setup 状态可用。
- 完成数据库选择后才能调用现有 `POST /api/v1/setup/complete` 创建第一个管理员。
- 数据库密码不写日志、错误详情、普通 API 响应或审计事件。

## 验收

- [ ] 空 `/config` 启动后可以打开数据库选择页面。
- [ ] 选择 SQLite 后，现有初始化、重启、migration 和全部业务测试保持通过。
- [ ] 选择 PostgreSQL 后，从空 PostgreSQL 数据库运行完整 schema migration，能创建管理员、登录并完成基本媒体库读写。
- [ ] PostgreSQL 连接失败、认证失败、非空非 Lux 数据库和无效配置均给出脱敏错误，且不会自动切换 SQLite。
- [ ] 已有 `/config/lux.db` 的实例不显示数据库选择，不改变已有 SQLite 数据。
- [ ] 前端显示 SQLite/PostgreSQL 选项、连接测试状态和失败原因；不回显密码。

## 验证

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `POSTGRES_TEST_DATABASE_URL=... cargo test --locked --test postgres_database -- --ignored`
- `pnpm --dir web test`
- `pnpm --dir web build`
- `uname -m`

## 预计文件

- `Cargo.toml`、`Cargo.lock`
- `src/config/`、`src/storage/`、`src/main.rs`
- `src/api/mod.rs`、`src/application/setup.rs`
- `migrations/` 与 PostgreSQL migration 目录
- `web/src/features/auth/SetupPage.tsx`、`web/src/lib/api/`
- `tests/setup.rs`、PostgreSQL 集成测试
- `docs/`、`README.md`、`docs/DEPLOYMENT.md`
