# LUX-183：Webhook 通知器实施计划

## 当前范围

第一版实现 Lux 原生出站 Webhook，不实现播放进度通知、用户通知中心、Email、Telegram 或完整 Emby Webhook
payload 兼容。

事件类型：`MEDIA_ADDED`、`MEDIA_REMOVED`、`SCAN_COMPLETED`、`SCAN_FAILED`、`METADATA_UPDATED`、`JOB_FAILED`。

## 当前实施状态

后端核心已完成并拆分为以下原子提交：

- `f96c392f`：事件类型、URL 校验和 HMAC-SHA256 签名基础。
- `012625be`：持久化事件、投递记录和后台 worker。
- `bde93835`：`Retry-After`、瞬态 HTTP 状态分类、失败状态和 IPv4-mapped IPv6 校验。
- `70402954`：显式删除和扫描缺失媒体的 `MEDIA_REMOVED`。
- `9e388279`：元数据刷新和任务失败事件。
- `d03b2bee`、`822f64ed`：payload 白名单、权限/CSRF/secret 权限和 lease 回归测试。

当前阶段门仍需在 person credits 并行改动稳定后重新运行完整 Rust 检查；该并行改动曾使工作树暂时无法编译，不能据此宣称全量检查通过。

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

## 验证

- 单元测试：事件序列化、字段脱敏、URL 校验、IP 分类、HMAC、重试分类。
- 集成测试：migration、管理员权限、CRUD、测试发送、localhost 接收器、失败重试和重启恢复。
- 项目检查：`cargo build --locked`、`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、
  `cargo clippy --locked --all-targets --all-features -- -D warnings`、`./scripts/check-all.sh`。
