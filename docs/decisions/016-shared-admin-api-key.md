# ADR-016：共享管理员 API Key

## 状态

已接受

## 决策

Lux 提供一个服务器级共享 API Key，采用 Emby API Key 的调用方式，接受 `X-Emby-Token`、
`X-Lux-Api-Key`、`Authorization: Bearer` 和兼容的 `api_key` 查询参数。该 Key 同时适用于 Lux
`/api/v1` 与已实现的 Emby 兼容路由，并按服务器管理员权限执行。

Key 不与某个管理员账户绑定。所有服务器管理员查看同一个当前 Key；轮换会使所有旧调用方立即失效。
由于服务端必须让管理员重新查看当前 Key，Key 持久化在 `/config/lux_admin_api_key` 的受限文件中，
而不是只保存不可逆哈希。生成、轮换和撤销必须使用原子写入，且明文不得进入数据库、日志或审计事件。

## 后果

- 脚本可使用 Emby 风格的 Key 调用 Lux 和兼容 API。
- Key 泄露等价于服务器管理员权限，首版不提供细粒度 scope。
- 共享 Key 无法区分具体管理员，审计只能标记为 `admin_api_key`。
- Key 管理接口只接受管理员 Web session 和 CSRF，防止 Key 自行轮换或读取。
