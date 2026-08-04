# LUX-083：多版本聚合实施记录

## 范围

- 以显式文件名规则识别清晰度和剪辑版，不根据相似标题自动合并。
- 4K/2160p、1080p、720p 等清晰度作为同一逻辑电影的不同 media_sources。
- Director's Cut、Extended Cut、Unrated、Theatrical 等明确剪辑标记生成独立逻辑条目。
- 保留稳定的默认 source，并通过 MediaSourceId 允许客户端选择其他 source。

## 已完成

- [x] ParsedMovieFilename 提取 edition_name 和 quality_label。
- [x] 电影扫描将清晰度写入 source，将明确剪辑版写入独立 item。
- [x] Episode identity 使用剧集、季度和集号稳定聚合同一集的多个文件，并将清晰度/HDR 标签写入 source。
- [x] Lux 目录和 Emby PlaybackInfo 返回版本标签。
- [x] 4K/1080p/Director's Cut 电影与 1080p SDR/2160p HDR 单集 fixture，以及显式 source 选择测试。

## 边界

- 当前不凭相似标题、文件大小或编码猜测版本关系。
- 当前尚未从在线 provider ID 自动聚合；在线识别完成后应使用 provider ID 或管理员确认再扩展聚合。
- 单集版本聚合依赖稳定的剧集目录、季度目录和 SxxEyy/1x02 编号；无法解析编号的文件仍不会自动合并。

## 验证

    cargo test --test scanner --test series_scanner --test playback
    cargo clippy --all-targets --all-features -- -D warnings
