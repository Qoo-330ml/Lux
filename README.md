# Lux

Lux 是面向 NAS 的个人媒体服务端，核心服务使用 Rust，Web 客户端使用同源 Vite + React + TypeScript 静态资源。
当前仓库处于工程骨架阶段，产品与实现边界以 [`docs/LUX-DEVELOPMENT.md`](docs/LUX-DEVELOPMENT.md) 为准。

## 开发环境

- Rust stable（仓库通过 `rust-toolchain.toml` 固定工具链频道）
- Node.js 与 pnpm（Web 阶段使用）
- Docker（容器阶段使用）

## Docker 快速启动

```bash
mkdir -p config media
docker compose up -d
```

首次启动后访问 `http://localhost:8097/` 完成初始化。Compose 默认把仓库旁的 `./config` 映射到容器 `/config`，把 `./media` 映射到 `/media`；这两个目录均为本地运行数据，不会提交到 Git。生产环境可直接修改 `compose.yaml` 中媒体挂载的宿主机路径，并应在反代 HTTPS 后使用。

镜像启动时会自动把 `/config` 交给 UID 10001 后再运行 Lux，因此首次 bind mount 不需要手动 `chown`。`media` 仍需由该 UID 读写，才能回写 NFO/图片（可用 NAS 的共享文件夹权限或 ACL 完成）。

生产部署、HTTPS 反代、Tailscale 和升级说明见 [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md)。

## Docker Hub 发布

`.github/workflows/dockerhub.yml` 会在 Pull Request 中只构建验证，在 `main` 推送和 `v*.*.*` 版本标签时分别使用 GitHub 原生 amd64/ARM64 runner 构建，再合并并发布 `linux/amd64`、`linux/arm64` manifest，不使用 QEMU。请在 GitHub 仓库的 Settings → Secrets and variables → Actions 中配置：

- `DOCKERHUB_USERNAME`：Docker Hub 用户名。
- `DOCKERHUB_TOKEN`：Docker Hub Access Token，不要使用账户密码。

镜像名为 `docker.io/<DOCKERHUB_USERNAME>/lux`。例如发布版本：

```bash
git tag v0.1.0
git push origin v0.1.0
```

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
