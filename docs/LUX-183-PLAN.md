# LUX-183：Webhook 通知器实施计划

## 当前范围

LUX-183 当前只实现一个出站 Webhook 渠道，沿用同一套持久化事件/投递记录：播放事件使用独立节流规则，
Lux 原生和 Emby 风格 payload 使用独立 DTO adapter，不污染 Lux 原生合同。Telegram、企业微信和 Email
暂不纳入当前交付范围。

基础事件类型：`MEDIA_ADDED`、`MEDIA_REMOVED`、`SCAN_COMPLETED`、`SCAN_FAILED`、`METADATA_UPDATED`、`JOB_FAILED`。
扩展事件类型：`PLAYBACK_STARTED`、`PLAYBACK_PAUSED`、`PLAYBACK_PROGRESS`、`PLAYBACK_STOPPED`。

## 后续完成标准

- 管理员可以在 Web 控制台配置 Webhook、查看投递记录和手动重试；Secret 不进入普通响应或日志。
- 播放开始、暂停、停止事件至少一次投递；播放进度按会话节流，乱序回调不会导致事件倒退或通知风暴。
- Webhook adapter 只接收已脱敏的统一事件，secret 受限存储，HTTP 超时、重试和错误分类沿用统一投递器。
- Emby adapter 与 Lux 原生 adapter 分离，固定事件/模板字段有脱敏协议回归；未覆盖的 Emby 插件行为不得宣称兼容。

## 当前实施状态

后端核心已完成并拆分为以下原子提交：

- `f96c392f`：事件类型、URL 校验和 HMAC-SHA256 签名基础。
- `012625be`：持久化事件、投递记录和后台 worker。
- `bde93835`：`Retry-After`、瞬态 HTTP 状态分类、失败状态和 IPv4-mapped IPv6 校验。
- `70402954`：显式删除和扫描缺失媒体的 `MEDIA_REMOVED`。
- `9e388279`：元数据刷新和任务失败事件。
- `d03b2bee`、`822f64ed`：payload 白名单、权限/CSRF/secret 权限和 lease 回归测试。
- `94d88cdd`：播放开始、暂停、节流进度和停止事件。
- `4fcab643`：Emby 风格 payload adapter、格式分组投递和兼容回归测试。

当前阶段门已在 person credits 并行改动稳定后完成；本机 ARM64 的 Rust 全量测试、fmt、clippy、Web 测试和构建均已通过。

## 实施增量

### 1. 事件合同和存储

- 增加版本化事件 envelope、允许的事件类型和字段白名单。
- 增加 `notification_destinations`、`notification_events`、`notification_deliveries`。
- 添加空库 migration、幂等约束和分页查询。

### 2. 管理 API

- 增加管理员 Webhook 目标 CRUD、启停、secret 轮换和测试发送。
- 所有写请求执行管理员权限和 CSRF 检查。
- URL 校验、私有网络显式开关和 secret 脱敏。

### 3. 投递 worker

- 有界并发、10 秒超时、无重定向、有限指数退避。
- 网络错误、429、5xx 重试；其他 4xx 进入失败状态。
- 投递 lease 在进程重启后可恢复。
- HMAC-SHA256 签名和 `eventId` 幂等头。

### 4. 事件接入

- 媒体新增/移除和扫描完成/失败接入通知 outbox。
- 元数据和后台任务失败事件接入。
- 重复扫描不重复产生新增事件。

当前交付不包含 Telegram、企业微信、Email 或 SMTP；后续如需增加，必须另立任务和 API/凭据设计。

## 验证

- 单元测试：事件序列化、字段脱敏、URL 校验、IP 分类、HMAC、重试分类。
- 集成测试：migration、管理员权限、CRUD、测试发送、localhost 接收器、失败重试和重启恢复。
- 项目检查：`cargo build --locked`、`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、
  `cargo clippy --locked --all-targets --all-features -- -D warnings`、`./scripts/check-all.sh`。
