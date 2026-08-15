# LUX-182 实施计划：Emby 风格共享管理员 API Key

## 目标

提供一个服务器级共享管理员 API Key。所有管理员看到同一个 Key；Key 调用按服务器管理员权限执行，
同时兼容 Lux API 和已实现的 Emby API。

## 安全边界

- Key 至少包含 256 bit 随机熵，保存于 `/config/lux_admin_api_key`，Unix 权限为 `0600`。
- Key 不写入数据库、日志、审计元数据、错误响应或 URL 之外的普通响应；管理页面只在管理员明确读取时返回。
- API Key 请求不需要 Cookie CSRF，但仍须通过服务器管理员权限和远程访问策略。
- API Key 不能调用自身的读取、轮换和撤销接口；这些操作必须使用管理员 Web session 和 CSRF。
- 共享 Key 不代表具体管理员身份，审计使用 `admin_api_key` 标记。

## 协议

- `X-Emby-Token: <key>`
- `X-Lux-Api-Key: <key>`
- `Authorization: Bearer <key>`
- `?api_key=<key>`，仅为 Emby/Lux 兼容性保留；请求路径日志不得包含查询字符串。

## 管理接口

- `GET /api/v1/admin/api-key`
- `POST /api/v1/admin/api-key/rotate`
- `DELETE /api/v1/admin/api-key`

## 实施增量

1. API Key 文件服务、生成/轮换/撤销/常量时间比较及服务单测。
2. Lux 与 Emby 鉴权入口接入共享 Key，覆盖 Cookie CSRF 边界和旧认证回归。
3. 管理接口、审计与 API 文档。
4. Web 管理页面、复制和轮换确认。
5. 完整项目检查与安全审查。
