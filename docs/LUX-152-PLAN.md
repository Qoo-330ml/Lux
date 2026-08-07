# LUX-152 计划：IP 归属地查询增强插件

## 目标

把 IP 归属地查询抽象为 Lux Plugin SDK v1 的统一网络插件能力，并将 Hiofd 和 ip138
分别作为可独立启停的插件运行。ip138 默认启用；安装其他归属地插件后停用 ip138。现有播放
会话仪表盘的异步行为和 24 小时缓存保持不变。

## 统一接口

manifest：

```json
{
  "type": "ip_location",
  "category": "NETWORK",
  "capabilities": ["ip.location"]
}
```

请求：

```json
{"method":"ip.location","params":{"ip":"8.8.8.8"}}
```

返回字段：

```json
{
  "ip": "8.8.8.8",
  "country": "美国",
  "province": "加利福尼亚州",
  "city": "山景城",
  "district": "Santa Clara",
  "street": "Amphitheatre Parkway",
  "isp": "Google",
  "latitude": null,
  "longitude": null
}
```

所有归属地字段均为可选文本；插件不能通过返回值携带凭据、调试信息或原始响应。
Lux 宿主在 RPC 边界重新验证 JSON、IP 一致性和字段长度，并计算仪表盘使用的 `location`。

## 插件注册与优先级

| 插件 ID | 显示名称 | 上游 | 优先级 |
| --- | --- | --- | --- |
| `org.lux.ip-hiofd` | IP归属地查询增强 | Hiofd `IpQuery` | 安装后优先 |
| `org.lux.qoo-ip138` | ip138 IP归属地查询 | ipshudi.com 页面 | 默认 |

插件只声明 `ip.location`，并实现 `plugin.hello`、`plugin.health`、`ip.location` 和
`plugin.shutdown`。宿主只尝试已安装且能力声明正确的插件；没有其他归属地插件时使用 ip138，
安装其他归属地插件后停用 ip138。

## 安全与可靠性边界

- 宿主只接收已经从可信播放会话得到的 IP；插件自身仍拒绝非法输入。
- 宿主拒绝私网、回环、链路本地、未指定和多播地址。
- Hiofd 的 `key11`、`pwd11` 只存在 Hiofd 插件进程；不进入 Lux API、日志、缓存或 SQLite。
- 第三方响应限制为 64 KiB；qoo-ip138 HTML 只解析受限表格文本，字段最多 256 字符。
- 成功结果缓存 24 小时，失败结果缓存 5 分钟，最多保留 256 个 IP，后台并发最多 8 个。
- 第三方服务故障、超时、非法响应和插件异常只产生未解析结果，不影响播放或仪表盘响应。
- 不允许插件访问 Lux SQLite、媒体库根目录或内部任务对象；网络请求只由插件实现。

## 实施切片

1. 扩展 manifest 校验和协议结构，并覆盖无效类型、能力和 IP 返回的测试。
2. 在 `PluginService` 中按 ip138 默认、其他已安装归属地插件替代的规则调用并校验结果。
3. 让 `IpLocationService` 依赖 `PluginService`，删除主进程 Hiofd HTTP 逻辑，保留缓存。
4. 将 `/Users/Qoo/Desktop/mywork/IP/IP-hiofd` 和 `qoo-ip138` 增加标准 JSON-RPC 插件入口、manifest 和测试。
5. 扩展包构建脚本和 Rust 集成测试，运行全量检查并记录 ARM 架构。

## 不在本任务内

- 不新增数据库迁移或公开 IP 查询端点。
- 不改变已有管理员仪表盘 JSON 字段。
- 不让 qoo-ip138 或 Hiofd 参与登录、ACL、播放地址或媒体扫描。
