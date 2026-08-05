# LUX-145：后台本地视频缩略图任务

## 目标

把 `ffmpegthumb` 的缩略图行为重写为 Lux 内置的后台能力。缩略图任务在一次媒体库扫描成功后运行，不增加独立插件进程，也不处理 `.strm` 指向的远程媒体。

## 行为契约

- 只从 `media_sources.source_kind = 'LOCAL_FILE'` 选择视频来源；`STRM_URL` 永远不进入 ffmpeg 调用。
- 每个逻辑媒体项优先使用默认本地来源，避免多版本重复生成。
- 默认截取 `00:03:01` 的第一帧；输出为同目录、按媒体文件名区分的 JPEG 缩略图。
- 已存在并可复用的缩略图不覆盖；缺失或原登记路径失效时才生成。
- 输出通过临时文件写入后原子重命名，并以 `THUMB`/`FFMPEG` 登记到 `item_images`。
- ffmpeg 使用参数数组调用，限制运行时间；源路径和输出路径均须位于已登记媒体库根目录内。
- 单个文件失败只记录失败计数和扫描任务事件，不阻塞其他媒体项；任务本身仍可随下一次扫描重试。

## 实现边界

- 复用现有持久化扫描任务作为后台宿主，扫描成功后记录 `THUMBNAIL_COMPLETED` 或 `THUMBNAIL_FAILED` 事件。
- 不新增用户请求路径扫描、ffprobe、转码、`.strm` 网络访问、缩略图管理 API 或 Web 页面。
- 不复制 MoviePilot 源码；只重写所需行为，避免引入其运行时依赖和 GPL 代码边界。

## 预计修改文件

- `docs/LUX-DEVELOPMENT.md`
- `src/application/mod.rs`
- `src/application/thumbnails.rs`
- `src/storage/mod.rs`
- `src/application/scanner.rs`
- `src/api/mod.rs`
- `tests/thumbnails.rs`

## 验收与验证

- 本地视频在扫描完成后生成缩略图并写入 `item_images`。
- `.strm` 只有同库本地来源时也不被 ffmpeg 调用；纯 `.strm` 媒体不生成缩略图。
- 已存在缩略图不会被覆盖。
- ffmpeg 失败不会阻止扫描任务完成，并产生失败事件/计数。
- 运行 `cargo test --locked --test thumbnails`、相关扫描测试、`cargo fmt --all -- --check` 和 `cargo clippy --locked --all-targets --all-features -- -D warnings`。

