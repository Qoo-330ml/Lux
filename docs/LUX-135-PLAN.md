# LUX-135：安全和故障恢复审查记录

## 已有证据

- [x] ACL：`tests/acl.rs` 覆盖列表、详情、图片、播放、字幕、下载和 Emby 路径的跨库拒绝。
- [x] 路径：`tests/playback.rs`、`tests/images.rs`、`tests/subtitles.rs` 和库根路径测试覆盖 canonicalize、根目录逃逸、符号链接/越界路径边界。
- [x] 令牌和密码：Argon2id 密码哈希、Web session/Emby token 哈希存储、CSRF 和登录限流已有认证测试。
- [x] NFO/图片：损坏 NFO、只读目录、并发修改、原子写入和图片类型/大小校验已有测试。
- [x] 代理头：`tests/remote.rs` 验证不可信 peer 的 forwarded header 不生效，trusted proxy 才能识别远端地址。
- [x] 日志/审计：请求 trace 只记录 URI path，并在 JSON span 中关联服务端 requestId、durationMs、statusCode 和 errorCode；管理员操作写入脱敏审计事件，不记录密码、Cookie、token 或外部 URL。`tests/observability.rs` 对实际进程日志做黑盒验证。
- [x] 故障边界：根路径暂时不可用、ffprobe 失败、TMDb 429/5xx/超时、NFO/图片写回失败均有可重试或隔离行为测试；`/api/v1` 的空 503 统一转换为带 `DATABASE_UNAVAILABLE` 和 requestId 的错误响应，Emby 兼容路由保持原协议边界。`tests/api_error_contract.rs` 覆盖两者。
- [x] 容器重启：ARM64 compose E2E 已验证迁移、媒体库、扫描条目和 Range 直放数据保持。
- [x] 本机 ARM64 强制终止恢复：`scripts/restart-recovery-smoke.sh` 在首批游标提交后发送 `SIGKILL`，复用同一 SQLite 数据目录重启后 5,000/5,000 条扫描任务完成；最新源码 revision `70474bc` 于 2026-08-03 重跑通过。
- [x] SQLite 写能力诊断：`Database::probe_write` 使用可回滚的真实事务；`/health/ready` 报告 `database_write_unavailable`，管理员健康接口报告 `database.status=degraded`、`database.writable=false`；查询只读连接、目录只读和正常路径均有测试。

## 尚未完成的故障注入

- [ ] 受控磁盘满场景和真实持久卷恢复报告；SQLite 写失败的在线诊断契约已完成，但尚未在 Docker/NAS 上注入磁盘耗尽。
- [ ] 真实 Tailscale/HTTPS 反代下的 trusted proxy、Range、超时和缓冲验证。
- [ ] 高风险问题复核、接受记录和发布候选签名。

本记录不把已有单元/集成测试当作真实 NAS 7 天运行或正式发布门的替代证据。
