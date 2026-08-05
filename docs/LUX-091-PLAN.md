# LUX-091 下载与管理权限

## 实现

- [x] 新增 Lux/Emby 下载端点。
- [x] 下载同时检查 `can_download` 和媒体库 ACL。
- [x] 下载复用 source ID、Range、路径 containment 和流式读取，不接受任意路径。
- [x] 成功响应添加 `Content-Disposition: attachment`。
- [x] 下载始终返回选中的单个 `LOCAL_FILE` 或 `STRM_URL` 资源，不再创建 ZIP 或合并同目录旁车文件。
- [x] Lux 与 Emby 下载端点共享单资源响应语义；`.strm` 下载读取其首个非空 URL，并由 Lux 请求和流式转发远程资源，不创建 ZIP。
- [x] `.strm` 远程下载支持鉴权后的 GET/HEAD、单 Range、上游响应状态和媒体响应头；不把入站认证头转发到上游。
- [x] `.strm` 远程下载只允许 HTTP/HTTPS，拒绝 URL 凭据、fragment、localhost、元数据主机和解析到私网/保留地址的主机；禁用自动重定向并设置连接/读取超时。

## 明确不做

- 下载权限不是 DRM；获准直放的用户理论上仍可保存已接收字节。
