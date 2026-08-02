# LUX-032：本地电影 NFO 与海报实施计划

## 范围

在 LUX-031 已发现的本地电影条目上读取 `movie.nfo` 或同名 NFO，更新本地标题、原名、年份和简介，并发现同目录 `poster`/`fanart` 图片。所有内容只来自本地文件，不调用 TMDb。

## 规则

- NFO 作为不可信 XML 处理，限制文件大小和事件数量；解析失败只记录失败状态，不阻塞条目。
- `movie.nfo` 与媒体同名 `.nfo` 均支持，优先使用 `movie.nfo`。
- 识别 `title`、`originaltitle`、`year`、`plot`/`overview`；缺失字段保留已有本地文件名结果。
- 图片只发现同目录常见 `poster`、`fanart` 文件，保存路径、大小和来源；不读取图片内容。
- 重复执行不重复插入同一条目同一图片类型/索引。

## 增量任务

### Slice 1：NFO parser 和图片 migration

- [x] 引入小型 XML parser 依赖并新增 `0007_item_images.sql`。
- [x] 实现受限 NFO parser，覆盖正常、部分、空和损坏 XML。
- [x] 实现安全的 poster/fanart 文件发现。

### Slice 2：本地 metadata enrichment

- [x] 根据已持久化 media source 找到媒体文件和同目录元数据。
- [x] 更新 media item 元数据，保存 item_images。
- [x] 重复运行幂等；损坏 NFO 生成失败报告但保留条目。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机用正常/损坏 NFO 和 poster/fanart fixture 验证持久化。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证正常、部分、空和损坏 NFO，以及图片元数据持久化和重复运行幂等。

## 明确不做

- 不调用 TMDb。
- 不做图片解码、缩放或海报 HTTP 端点。
- 不修改原始 NFO 或图片文件。
