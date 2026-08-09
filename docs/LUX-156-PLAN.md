# LUX-156：持久化日志与管理员导出

## 目标

保留 Lux 的 JSON stdout 容器日志，同时将同一份结构化日志按 UTC 日期持久化到
`<config>/logs/lux.YYYY-MM-DD.log`，并允许管理员从控制台按日期导出 ZIP，便于分析扫描、图片和
请求错误。

## 约束

- 使用独立后台日志 writer，不能让每条日志在 Tokio 核心 worker 上同步写磁盘。
- 文件日志与 stdout 使用同一套 `RUST_LOG` 过滤级别和 JSON 格式。
- 导出接口只允许 `canManageServer` 管理员，日期输入严格为 `YYYY-MM-DD`。
- 默认导出最近 7 个 UTC 日；显式范围最多 31 个 UTC 日；不读取或打包配置目录中的其他文件。
- 继续遵守日志脱敏规则，不输出密码、session/Emby token、Cookie、TMDb 凭据或完整外部 URL。

## 实现切片

1. `tracing-appender` 日滚动 writer 与启动生命周期 guard，失败时 stdout 降级。
2. 日志导出服务和管理员 ZIP API，加入权限、日期范围和文件读取测试。
3. 管理员任务与日志页添加 UTC 日期范围和直接下载按钮。
4. 完成 Rust/Web 检查、黑盒日志验证和代码审查。

## 已知边界

本任务不添加自动日志保留清理；管理员或部署系统负责根据配置卷容量管理历史日文件。
