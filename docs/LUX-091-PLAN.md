# LUX-091 下载与管理权限

## 实现

- [x] 新增 Lux/Emby 下载端点。
- [x] 下载同时检查 `can_download` 和媒体库 ACL。
- [x] 下载复用 source ID、Range、路径 containment 和流式读取，不接受任意路径。
- [x] 成功响应添加 `Content-Disposition: attachment`。

## 明确不做

- 下载权限不是 DRM；获准直放的用户理论上仍可保存已接收字节。
