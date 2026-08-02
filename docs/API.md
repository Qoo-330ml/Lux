# Lux API（当前实现）

Lux 自有 API 使用 `/api/v1`，响应字段使用 camelCase。错误统一为：

```json
{
  "error": {
    "code": "AUTHENTICATION_REQUIRED",
    "message": "需要登录",
    "requestId": "..."
  }
}
```

## 初始化

- `GET /api/v1/setup/status`：返回 `initialized`。
- `POST /api/v1/setup/complete`：仅在没有用户时创建首个管理员；成功返回 201，重复或并发失败返回 `SETUP_ALREADY_COMPLETED`。

请求体至少包含 `username` 和 `password`，可选 `displayName`。密码只以 Argon2id PHC 哈希形式写入数据库。

## Web 会话

- `POST /api/v1/auth/login`：校验用户名和密码，成功后设置 `lux_session` 与 `lux_csrf` cookie。
- `GET /api/v1/auth/me`：读取当前 Web session，返回用户和权限。
- `POST /api/v1/auth/logout`：需要有效 `lux_session` 和 `X-CSRF-Token`，成功返回 204 并撤销 session。

`lux_session` 为 `HttpOnly; Secure; SameSite=Lax; Path=/`，数据库只保存其 SHA-256 哈希。`lux_csrf` 不设置 HttpOnly，供同源 Web 客户端读取并通过 `X-CSRF-Token` header 发送；数据库保存 CSRF 哈希。session 有效期为 30 天，注销后立即失效。

当前阶段的 cookie 始终标记 `Secure`，部署时应使用 HTTPS；本机 HTTP 集成测试只验证协议和服务端行为，不代表浏览器会在不安全来源发送 Secure cookie。

## 媒体库管理（LUX-030）

以下接口要求有效 Web session；写操作还要求 `X-CSRF-Token`，并检查当前用户的 `canManageServer` 权限：

- `GET /api/v1/admin/libraries`：列出媒体库及其根路径。
- `POST /api/v1/admin/libraries`：创建媒体库。请求体为 `{ "name": "Movies", "kind": "MOVIE", "realtimeWatchEnabled": false }`，`kind` 支持 `MOVIE`、`SERIES`、`MIXED`。
- `POST /api/v1/admin/libraries/{libraryId}/roots`：添加根路径。请求体为 `{ "path": "/media/movies" }`。

根路径会先 canonicalize，再检查目录存在且可读；`isWritable` 独立返回。只读目录可以保存，但返回 `LIBRARY_PATH_NOT_WRITABLE` 警告。同一库的重复/重叠路径分别返回冲突/不可处理实体错误，跨库重叠返回结构化警告。

## Emby 认证（LUX-024）

- `GET /Users/Public`：返回未禁用用户的公开登录信息。
- `POST /Users/AuthenticateByName`：读取 `Username`/`Pw`，解析 `Authorization: Emby Client=..., Device=..., DeviceId=..., Version=...`，返回 `AccessToken`、`User`、`SessionInfo` 和 `ServerId`。
- `POST /Sessions/Logout`：接受 `X-Emby-Token` 或 `api_key`，撤销对应 token，成功返回 204。
- `System/Info` 和 `System/Ping`：需要有效的 `X-Emby-Token` 或 `api_key`；`System/Info/Public` 不要求认证。

Emby access token 与 Web session 完全分离。access token 是高熵随机值，只在认证响应中返回；数据库只保存 SHA-256 哈希以及设备元数据。认证失败响应不区分“用户不存在”和“密码错误”。

## 当前边界

上述接口是 LUX-021/LUX-022 的基础能力。媒体库、Emby 兼容、用户管理和进度接口按开发规格后续任务逐项增加；未实现端点不应被客户端兼容性声明引用。

## 电影查询（LUX-034）

Lux 电影查询要求有效 Web session：

- `GET /api/v1/libraries`：返回已启用媒体库的基本信息，不暴露服务器路径。
- `GET /api/v1/libraries/{libraryId}/items?page=1&pageSize=50`：按稳定标题顺序分页返回电影。
- `GET /api/v1/items/{itemId}`：返回电影详情、媒体源和已探测轨道。
- `GET|HEAD /api/v1/items/{itemId}/images/{type}`、`/{type}/{index}`：读取本地 poster/fanart，支持 ETag 和 `If-None-Match`。

Emby 电影查询要求有效 `X-Emby-Token` 或 `api_key`：

- `GET /Users/{userId}/Views`：返回电影媒体库视图。
- `GET /Users/{userId}/Items`、`GET /Items`：支持 `ParentId`、`StartIndex`、`Limit` 和 `IncludeItemTypes=Movie`，默认从 0 开始、每页 50 条，单页上限 100。
- `GET /Users/{userId}/Items/{itemId}`、`GET /Items/{itemId}`：返回 Emby 兼容电影详情 DTO。
- `GET|HEAD /Items/{itemId}/Images/{Type}`、`/{Type}/{Index}`：读取与 Lux API 相同的本地图片记录，支持 `X-Emby-Token` 或 `api_key`。
- `GET /Users/{userId}/Items/Resume`：当前返回空的继续观看列表。

媒体 DTO 只返回客户端所需的标题、年份、简介、时长、容器、大小、码率和轨道信息，不返回服务器内部文件路径。图片内容端点属于 LUX-035。

## Emby 连接探针（LUX-023）

以下端点同时接受根路径和 `/emby` 前缀：

- `GET /System/Info/Public`
- `GET /System/Info`
- `GET|POST /System/Ping`

响应只返回 Lux 名称、版本、持久 ServerId 和必要能力字段，不返回配置目录、数据库路径或其他内部路径。LUX-023 的自动化测试是本地协议 shape 测试；VidHub、SenPlayer 和 Infuse 的真实连接证据要到 LUX-025 记录。
