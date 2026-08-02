# LUX-071 PlaybackInfo 和版本选择基础

## 范围

为可访问媒体条目提供 Emby `PlaybackInfo`，只声明本地文件的 DirectPlay 能力，并返回稳定媒体源 ID、媒体流和受控直放 URL。

## 实现

- [x] 新增 `GET|POST /Items/{itemId}/PlaybackInfo`，支持 `api_key` 和 `MediaSourceId`。
- [x] 只声明 `SupportsDirectPlay=true`，`SupportsDirectStream=false`、`SupportsTranscoding=false`。
- [x] 直放 URL 使用 `/Videos/{itemId}/{mediaSourceId}/stream.{container}`，不返回服务器内部路径。
- [x] 默认 source 按 `is_default DESC, source_id ASC` 选择，显式 source ID 只返回指定源。
- [x] 详情 DTO 的 MediaSources 同步返回 source ID、能力字段、直放 URL和媒体流。

## 验证

- Emby contract 集成测试验证默认 source、能力声明、URL shape 和 query token。
- 与 LUX-070 的全量 ARM64 回归保持通过。

## 明确不做

- 本阶段不代理或验证 `.strm` 外部 URL；该行为属于 LUX-072。
- 本阶段不实现转码、音视频重封装或 DirectStream。
