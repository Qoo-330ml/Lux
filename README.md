# Lux

Lux 是面向 NAS 的个人媒体服务端，核心服务使用 Rust，Web 客户端当前使用同源浏览器原生 ES module，后续再按规格演进构建链。
当前仓库处于工程骨架阶段，产品与实现边界以 [`docs/LUX-DEVELOPMENT.md`](docs/LUX-DEVELOPMENT.md) 为准。

## 开发环境

- Rust stable（仓库通过 `rust-toolchain.toml` 固定工具链频道）
- Node.js 与 pnpm（Web 阶段使用）
- Docker（容器阶段使用）

## Docker 快速启动

```bash
docker compose up --build
```

首次启动后访问 `http://localhost:8097/` 完成初始化。媒体目录默认挂载为 `./media`，数据和迁移数据库持久化在 `lux-data` 卷中；生产环境应在反代 HTTPS 后使用，并谨慎配置 `LUX_TRUSTED_PROXY_CIDRS`。

生产部署、HTTPS 反代、Tailscale 和升级说明见 [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md)。

## 常用命令

```bash
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
./scripts/check-all.sh
```

实现按开发规格中的 LUX 任务逐项推进。每个任务完成后必须先通过自己的验收和阶段门，不能提前实现后续阶段。
