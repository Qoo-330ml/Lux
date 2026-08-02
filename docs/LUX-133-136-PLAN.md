# LUX-133 至 LUX-136：容器与发布阶段记录

## 已完成的基础切片

- [x] 多阶段 Dockerfile：Rust release builder 和 Debian runtime。
- [x] runtime 使用非 root `lux` 用户，包含 `ffprobe`、Web 静态资源和 `/health/live` 健康检查。
- [x] compose 提供 `/data` 持久化卷、媒体挂载、端口和 trusted proxy/TMDb 配置入口。
- [x] compose 媒体挂载使用读写模式，满足 NFO/图片回写要求。

## 已验证

- [x] 本机 ARM64 Docker build/启动健康检查：`lux:arm64-local` 为 `arm64/linux`，非 root `uid=10001`，`ffprobe` 可执行，容器健康检查通过。
- [x] 全新数据卷初始化、扫描、媒体 Range、容器重启后迁移/数据保持的 compose E2E：健康检查、setup、媒体库、1 条 MP4 直放 Range（206/100 bytes）和重启后库/条目均通过。

## 未完成的发布门

- [ ] amd64 构建产物和镜像签名/版本发布。
- [ ] Tailscale/反代 HTTPS、Range 缓冲和 trusted proxy 实机验证。
- [ ] 磁盘满、媒体挂载丢失、TMDb 故障和强制终止恢复演练。
- [ ] 飞牛 NAS 7 天运行和正式发布候选验收。
