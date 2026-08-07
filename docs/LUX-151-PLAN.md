# LUX-151 计划：播放会话 IP 归属地

## 目标

参考 `/Users/Qoo/Desktop/mywork/IP/IP-hiofd`，在 Lux 中增加 Hiofd IP 归属地解析能力，并在管理员仪表盘的正在播放卡片显示已解析的归属地。

## 接口与边界

- Hiofd 请求只使用固定的 `https://toola.hiofd.com/router/rest` 地址和固定服务标识；不接受用户提交的上游 URL。
- Hiofd 的公开协议字段按参考项目内置为 `key11` 和 `pwd11`；它们不会返回 API、写入日志或持久化到数据库。
- 查询输入只来自 Lux 已按可信代理规则确定的播放会话 `remoteIp`。
- 只查询公网 IPv4/IPv6；私网、回环、链路本地、未指定和多播地址直接跳过。
- `GET /api/v1/admin/dashboard` 的每个 `nowPlaying` 项增加可空 `remoteIpLocation`：`location`、`district`、`street`、`isp`。解析未完成或失败时为 `null`。
- API 请求只读取内存缓存并安排后台工作，不等待 Hiofd；完整第三方响应、请求签名和任何凭据不得进入日志或 SQLite。

## 实施切片

1. 增加 Hiofd 协议客户端、响应校验、地址过滤、缓存/并发控制及单元测试。
2. 接入 AppState 和管理员仪表盘 DTO，增加 API 回归测试。
3. 接通现有 Web 占位展示，更新类型和组件测试。

## 风险与缓解

| 风险 | 缓解 |
|---|---|
| 第三方接口不可用或变更 | 后台异步、超时、失败 TTL、播放数据不依赖解析结果 |
| 客户端 IP 隐私泄露 | 不持久化、不记录、不向普通用户暴露，仅管理员仪表盘读取 |
| 被异常响应耗尽内存 | 固定响应上限、JSON 字段白名单、缓存条目和并发上限 |
| 代理/伪造 IP 误导归属地 | 复用 Lux 的可信代理地址解析，服务端不直接信任转发头 |

## 验证

```bash
uname -m
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
pnpm --dir web test
pnpm --dir web build
```

本地 ARM64 验证不代表目标飞牛 NAS/x86_64 的性能；Hiofd 真实上游可用性只作为手工 smoke 记录，不作为单元测试依赖。
