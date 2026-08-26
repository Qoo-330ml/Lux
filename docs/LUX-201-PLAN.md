# LUX-201 实施计划：TMDb/豆瓣与 Lux 主程序彻底解耦

## 目标

Lux 主程序只提供 provider-neutral 的 metadata 业务和 RPC 宿主能力。TMDb、豆瓣的 HTTP client、上游
DTO、凭据、语言/回退策略和图片 URL 处理留在 `/Users/Qoo/Desktop/mywork/Lux-plugins` 的独立进程。

不改变：metadata RPC v1 方法名和主要字段、SQLite provider ID JSON 存储、NFO/Emby 兼容 namespace、
LUX-200 已验证的后台队列与并发边界。

## 依赖关系

```text
manifest/provider contract
        ↓
plugin config isolation + legacy migration
        ↓
generic runtime/catalog/selection
        ↓
remove Lux TMDb implementation
        ↓
plugin releases/catalog + full regression
```

## 增量任务

### 1. 契约与记录（Lux）

文件：`docs/LUX-DEVELOPMENT.md`、`docs/LUX-201-PLAN.md`、`docs/PLUGIN-SDK.md`、
`docs/API.md`、`docs/decisions/028-provider-implementation-decoupling.md`。

验收：manifest 的 `providerKey`/`aliases`、专属配置路径、一次性旧配置迁移和兼容边界有明确文本；
metadata RPC v1 无破坏性变更。

### 2. 插件先行（Lux-plugins）

文件：TMDb/豆瓣入口、TMDb/豆瓣配置模块、两个 manifest、`plugins.json`、生成的 `index.json`。

验收：插件只读取 `LUX_PLUGIN_CONFIG_PATH`；没有该变量时安全使用默认值；TMDb 升至 `0.1.9`、豆瓣升至
`0.1.4`；测试覆盖配置路径不越界、旧字段解析和 RPC v1。

### 3. 宿主运行时（Lux）

文件：`src/application/plugin_protocol.rs`、`src/application/plugin_runtime.rs`、
`src/application/plugins.rs`、`src/application/scraper.rs` 及对应测试。

验收：aliases 由 manifest/catalog 通用解析；metadata 进程仅收到专属路径；不存在 metadata 专用的
TMDb 判断分支；旧 `tmdb` alias 仍能解析到 `org.lux.tmdb`。

### 4. 删除内置实现（Lux）

文件：删除 `src/application/tmdb.rs`、`src/application/tmdb_plugin.rs`，并更新 `src/application/mod.rs`、
`src/api/mod.rs`、`src/application/identification.rs`、`src/application/metadata.rs`、
`src/application/candidates.rs`、`src/application/network_diagnostics.rs` 和测试夹具。

验收：核心服务不再创建内置 scraper；候选、图片、人物、合集和重新识别只依赖 `ScraperProvider`/字符串
provider；兼容模块仍保留 NFO/Emby 的 provider ID 语义。

### 5. 发布与回归

验收：插件仓库通过单测、manifest 校验、Linux x86_64/aarch64 构建和 RPC 集成；Lux 通过完整 Rust/Web
检查、兼容性测试和 LUX-200 性能复测。记录 `uname -m`，不把本机 ARM64 数据外推到 NAS/x86_64。

## 风险与回滚

- 旧用户的 TMDb 凭据可能只存在共享文件：先复制到插件专属配置并保留旧文件一个发布周期，迁移失败时
  不删除旧文件。
- 历史数据中的 `tmdb`/`douban` 不是实现耦合：保留字符串兼容层和 alias，不修改数据库 schema。
- 删除主程序实现前必须通过插件 RPC 端到端验证；任何失败都回滚到上一个原子提交。

## 质量门

每个增量独立提交并验证。最终执行：

```text
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
pnpm --dir web test
pnpm --dir web build
```

插件仓库另行执行其 Rust 构建、manifest/index 校验、RPC 测试和两个 Linux 目标包构建。
