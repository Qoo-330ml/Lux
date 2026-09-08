# ADR-041：Lux Prism 一次性设备配对

## 状态

已接受

## 日期

2026-09-09

## 背景

Lux Prism 需要在不让用户手工复制 Emby AccessToken 的情况下连接 Lux。二维码是跨桌面
应用与 Web 的方便入口，但二维码内容和兑换接口都处在高价值凭据边界：票据不能被重放，
服务端不能把明文 secret 或 AccessToken 写入持久化存储，Web 的共享 API Key 也不能获得
设备配对能力。

现有 Lux 已经有 Web session/CSRF 认证和 Emby AccessToken 存储。`access_tokens` 的设备
元数据缺少客户端平台字段，因此新增可空 `device_type`，以保持已有令牌和普通 Emby 登录
向后兼容。

## 决策

### Web 创建，设备兑换

- 已登录 Web 用户通过 `POST /api/v1/auth/device-pairings` 创建 5 分钟票据。
- 创建和取消必须是 Web session + CSRF；共享管理员 API Key 不适用于这两个接口。
- Web 负责将当前 HTTP(S) origin、票据 ID、secret 和过期时间组成版本化
  `lux-prism://pair?v=1...` URI。服务端不保存完整 URI。
- Prism 仅把扫码后的服务器地址和名称展示给用户确认，然后向该 Lux 地址兑换。
- `POST /api/v1/device-pairings/{id}/redeem` 不要求 Web session，只接受票据 secret 与
  Prism 的设备元数据。

### 哈希与原子消费

secret 和 AccessToken 都使用密码学随机值；数据库只保存 SHA-256 哈希。兑换在一个短事务
中完成：读取并校验票据、原子标记 `consumed_at`、插入 `access_tokens`，任一步失败都回滚。
SQLite 使用已有 `BEGIN IMMEDIATE` 写事务，PostgreSQL 使用数据库事务和条件更新抵御并发
兑换。错误响应不会返回 secret、token 或完整 URI。

兑换创建的令牌沿用现有 Emby 令牌模型，`client_name` 固定为 `Lux Prism`，用户提交的
`deviceId`、`deviceName`、`platform` 和 `version` 分别映射到设备元数据；`platform` 写入
新增可空 `device_type`。

### 边界保护

- 设备字段在 HTTP 边界校验非空和长度，兑换请求体上限为 16 KiB。
- 创建按用户/来源地址限流，兑换按来源地址限流，均使用有界内存滑动窗口；返回
  `429` 和最多 60 秒的 `Retry-After`。
- 配对 ID 必须是 UUID；错误 secret、过期、取消和已消费票据有稳定机器码，但不暴露任何
  持久化 secret 或 AccessToken。

## 未采用方案

### 直接让 Prism 使用 Web session

被拒绝。桌面客户端不应持有 Web CSRF 会话，也不能把浏览器 Cookie 生命周期和客户端设备
生命周期耦合。

### 二维码内嵌长期 AccessToken

被拒绝。二维码可能出现在屏幕截图、浏览器历史或日志中；一次性短期 secret 能缩小泄露
窗口，并且令牌只在兑换成功响应中返回。

### 让共享 API Key 创建票据

被拒绝。共享 Key 是服务器级管理凭据，把它转换为设备令牌会扩大权限和审计边界；配对
必须绑定具体 Web 用户。

## 后果

- Prism 获得一次低摩擦的 Lux 连接入口，兑换后的令牌能复用现有 Emby 协议。
- 需要维护一次性票据迁移、限流状态和并发事务测试。
- `device_type` 是可空的附加字段；旧数据库升级后已有令牌保持有效，普通 Emby 登录无需
  改变请求合同。
- 撤销令牌仍遵循现有 AccessToken 撤销路径；设备配对本身不提供离线写操作。

## 验证

- 集成测试覆盖 session/CSRF、共享 API Key 拒绝、secret 哈希、过期/取消/重放、取消权限、
  限流和兑换后 Emby API 访问。
- 并发兑换测试确认同一票据最多一个成功，事务失败不留下令牌或已消费票据。
- SQLite 和 PostgreSQL 迁移均从空库和已有库验证；不在本机 ARM64 结果上推断其他平台。
