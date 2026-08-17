# ADR-011：通知器使用独立进程插件

## 状态

已接受。

## 决策

Lux 只维护统一的脱敏通知事件、outbox、投递 lease、重试和审计状态。每个通知目标绑定一个 provider
plugin。provider 通过 `type: "notification"`、`category: "NOTIFICATION"` 和
`notification.send` 能力接收统一 RPC 请求，负责平台专用 payload、认证和请求协议；宿主只处理统一结果。

历史 Webhook 目标使用 `builtin.webhook` 兼容 provider，保留现有 URL、HMAC、SSRF 和 Emby payload 行为，避免
一次迁移破坏现有配置。未来 Webhook、Telegram、企业微信等均可作为独立插件包发布，不需要在通知核心增加
平台分支。

通知插件通过 stdin/stdout RPC 接收目标配置和单个受控 secret；启动时不注入完整 `LUX_CONFIG_DIR`，因此不能
直接读取其他插件或系统 Secret。Secret 不进入数据库普通字段、事件 payload、API 列表或日志。

## 后果

- 通知队列和事件合同稳定，新增渠道不需要改动扫描、播放或存储核心。
- provider 插件崩溃、超时和返回的 retryable 结果统一进入宿主退避流程。
- 首版仍需要维护 `builtin.webhook` 兼容实现；独立 Webhook/Telegram/企业微信包可在后续仓库中实现。
- 插件进程隔离不是操作系统级沙箱；插件包和权限声明仍需经过现有安装校验。
