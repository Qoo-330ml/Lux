# LUX-035：本地海报兼容端点实施计划

## 范围

读取 LUX-032 已保存的 `item_images` 记录，为 Lux 和 Emby 提供同一份 poster/fanart 图片内容。端点只接受 item/type/index，不接受客户端磁盘路径。

## 规则

- GET 和 HEAD 共用认证、路径解析、ETag 和错误逻辑；HEAD 不返回文件体。
- 图片记录先从数据库按 item/type/index 查询，再 canonicalize 实际文件，并验证位于该媒体库的 canonical 根目录内。
- 符号链接越出媒体根、目录、未知扩展和超大文件均拒绝；不把任意路径交给客户端。
- ETag 由实际文件大小和修改时间生成，`If-None-Match` 命中返回 304。
- Lux 使用 Web session；Emby 使用 `X-Emby-Token` 或 `api_key`，二者读取同一图片记录。
- Emby DTO 的 `ImageTags`/`BackdropImageTags` 暴露已有图片记录的稳定 tag。

## 增量任务

### Slice 1：图片解析与服务

- [x] 增加 item image 候选查询和根路径校验服务。
- [x] 实现受限图片响应、MIME、Content-Length、ETag 和 304。
- [x] 将 Emby DTO 图片 tag 连接到本地图片记录。

### Slice 2：双端点与安全测试

- [x] 实现 Lux/Emby GET/HEAD 图片端点。
- [x] 覆盖 200、304、404、403、路径穿越和 API key 认证。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证同一 poster 通过 Lux/Emby 端点读取，HEAD 无 body，ETag 命中 304。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证同一图片的 Lux/Emby GET、HEAD、ETag/304、API key 和根路径越界拒绝。

## 明确不做

- 不解码、缩放或重新编码图片。
- 不接受客户端提交的绝对磁盘路径。
- 不实现图片下载、写回或多 Range。
