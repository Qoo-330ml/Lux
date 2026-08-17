# ADR-017：持久化出站 Webhook 通知

- 状态：已接受
- 日期：2026-08-16

## 决定

Lux 使用独立的持久化通知事件和投递记录实现出站 Webhook。业务服务只在数据库中记录事件和匹配的投递，
由有界后台 worker 发送 HTTP 请求。Webhook 使用 Lux 自有、带版本号的 JSON 合同；Emby Webhooks 插件的
payload 兼容作为后续独立适配层，不污染 Lux 内部领域模型。

通知目标只允许服务器管理员配置。目标 secret 存在 `/config` 受保护的 secret 文件中，不通过普通 API 返回，
请求使用 HMAC-SHA256 签名。投递语义为至少一次，接收方使用 `eventId` 幂等；Lux 不承诺 exactly-once。

## 原因

直接在扫描或播放请求中调用外部 HTTP 服务会让第三方故障拖慢前台功能，也无法可靠处理服务重启、超时和重试。
进程内广播只适合管理员 SSE，不能作为外部通知的事实来源，因为它不持久化且可能丢帧。

## 安全边界

- Webhook URL 是管理员输入，必须校验 scheme、凭据、查询参数、重定向和解析后的目标地址。
- 默认拒绝 loopback、私有、链路本地、metadata、未指定和 multicast 地址；允许局域网目标必须显式配置。
- 事件 payload 采用字段白名单，不包含本地路径、`.strm` 原始地址、token 或完整外部 URL。
- 日志、审计和 API 响应不包含 secret、签名原文、完整 URL 或完整 payload。

## 后果

- SQLite/PostgreSQL 需要通知表 migration。
- worker 需要处理投递 lease、有限重试、429/5xx 和失败保留。
- 配置 API、Web 管理页面和后续 Emby payload adapter 都必须遵守此边界。
