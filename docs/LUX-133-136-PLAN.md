# LUX-133 至 LUX-136：容器与发布阶段记录

## 已完成的基础切片

- [x] 多阶段 Dockerfile：Rust release builder 和 Debian runtime。
- [x] runtime 使用非 root `lux` 用户，包含 `ffprobe`、Web 静态资源和 `/health/live` 健康检查。
- [x] compose 提供 `/data` 持久化卷、媒体挂载、端口和 trusted proxy/TMDb 配置入口。
- [x] compose 媒体挂载使用读写模式，满足 NFO/图片回写要求。

## 已验证

- [x] 本机 ARM64 Docker build/启动健康检查：`lux:arm64-local` 为 `arm64/linux`，非 root `uid=10001`，`ffprobe` 可执行，容器健康检查通过。
- [x] 全新数据卷初始化、扫描、媒体 Range、容器重启后迁移/数据保持的 compose E2E：健康检查、setup、媒体库、1 条 MP4 直放 Range（206/100 bytes）和重启后库/条目均通过。
- [x] 当前源码重建 ARM64 镜像后再次验证：`arm64/linux`、`uid=10001`、`ffprobe`、`/health/ready`；setup 携带 TMDb token 和首个 `/media` 媒体库返回 `201`，配置文件权限为 `0600`。
- [x] 当前源码构建带版本和 revision label 的 ARM64 镜像 `lux:0.1.0-arm64-local`；`org.opencontainers.image.version=0.1.0`、revision=83b5977，非 root uid=10001、ffprobe、`/health/live`、`/health/ready` 和管理员健康写能力均已验证。
- [x] 当前 ARM64 镜像受控磁盘满/恢复演练：`scripts/disk-write-fault-smoke.sh` 在 64 MiB tmpfs 上验证 100% 满盘诊断、结构化写错误和释放空间后的恢复；镜像 revision `83b5977`。
- [x] 当前 ARM64 镜像受控媒体挂载不可访问/恢复演练：`scripts/mount-loss-smoke.sh` 验证 root 不可用时的扫描隔离、已有条目保留以及恢复后的重新探测；镜像 revision `83b5977`。
- [x] 当前 ARM64 本机 Nginx HTTPS 反代补充烟测：`scripts/proxy-smoke.sh` 验证自签名 TLS、可信代理公网地址访问控制和 Range 响应头保留；真实 Tailscale/HTTPS 实机验证仍保持待办。
- [x] 当前 ARM64 容器扫描后 ffprobe/PlaybackInfo 烟测：`scripts/probe-smoke.sh` 验证有效 MP4 扫描后自动进入 `READY`，返回 `RunTimeTicks` 和 2 条媒体流，并记录 `PROBE_COMPLETED`；镜像 revision `fc677a5`；真实 NAS 长期运行仍保持待办。

## 未完成的发布门

- [x] 本机 ARM64 Docker builder 交叉构建当前源码 `linux/amd64` 镜像 `lux:amd64-local`；revision `fc677a5` 的镜像架构、非 root `uid=10001`、`ffprobe` 和 `/health/live` 已验证。
- [ ] 镜像签名/版本发布。
- [ ] Tailscale/反代 HTTPS、Range 缓冲和 trusted proxy 实机验证。
- [ ] 真实 NAS 磁盘满、媒体挂载丢失和长期运行恢复演练；TMDb 故障与强制终止已有本机证据。
- [x] 已有根路径丢失、TMDb 故障、NFO/图片/ffprobe 故障和容器重启证据已汇总到 `docs/LUX-135-PLAN.md`。
- [ ] 飞牛 NAS 7 天运行和正式发布候选验收。
