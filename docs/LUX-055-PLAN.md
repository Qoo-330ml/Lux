# LUX-055 图片下载和原子写回

## 目标

在管理员后续选择在线元数据时，为缺失的 poster/fanart 下载受信任的图片内容，安全写入媒体目录，并让 `item_images` 索引立即指向新文件。

## 已实现

- `ImageWriteService` 使用独立 HTTP 客户端和可配置超时、最大响应体大小。
- 仅接受无凭据的 `http`/`https` URL，以及 `image/jpeg`、`image/png`、`image/webp`。
- 同时校验响应大小和图片内容签名：JPEG SOI/EOI、PNG 签名/IEND、WebP RIFF/WEBP。
- 根据已有 poster/fanart 扩展名复用目标；缺失时使用响应类型对应的扩展名。
- 媒体根路径 canonicalize，拒绝根外路径和符号链接目标。
- 图片写回使用同目录唯一临时文件、`sync_all`、原子 rename、目录刷盘和失败清理。
- 写回成功后通过唯一键 upsert `item_images`，刷新路径、大小、内容 SHA-256 标签和 `TMDB` 来源。

## 验证

- stub 图片服务覆盖 poster 与 fanart 下载、扩展名选择和索引更新。
- 损坏内容、错误 MIME、超限响应均验证为失败，且不留下目标文件或索引记录。
- 独立原子写回测试确认临时文件不会残留。

## 明确不做

- 本阶段不接入真实 TMDb 图片域名、不开放管理员 API；候选选择和端到端重新识别由 LUX-056 接入。
- 本阶段不实现图片缩放、缓存淘汰或透明 Logo/banner 等扩展图片类型。
