# LUX-091 下载与管理权限

## 实现

- [x] 新增 Lux/Emby 下载端点。
- [x] 下载同时检查 `can_download` 和媒体库 ACL。
- [x] 下载复用 source ID、Range、路径 containment 和流式读取，不接受任意路径。
- [x] 成功响应添加 `Content-Disposition: attachment`。
- [x] 下载始终返回选中的单个 `LOCAL_FILE` 或 `STRM_URL` 文件，不再创建 ZIP 或合并同目录旁车文件。
- [x] Lux 与 Emby 下载端点共享文件响应语义；`.strm` 下载本地指针文件，不请求或代理其中的外部 URL。

## 明确不做

- 下载权限不是 DRM；获准直放的用户理论上仍可保存已接收字节。
