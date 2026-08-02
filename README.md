# Lux

Lux 是面向 NAS 的个人媒体服务端，核心服务使用 Rust，Web 客户端按规格使用 React + TypeScript。
当前仓库处于工程骨架阶段，产品与实现边界以 [`docs/LUX-DEVELOPMENT.md`](docs/LUX-DEVELOPMENT.md) 为准。

## 开发环境

- Rust stable（仓库通过 `rust-toolchain.toml` 固定工具链频道）
- Node.js 与 pnpm（Web 阶段使用）
- Docker（容器阶段使用）

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

