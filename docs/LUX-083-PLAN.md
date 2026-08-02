# LUX-083：多版本聚合实施记录

## 范围

- 以显式文件名规则识别清晰度和剪辑版，不根据相似标题自动合并。
- 4K/2160p、1080p、720p 等清晰度作为同一逻辑电影的不同 media_sources。
- Director's Cut、Extended Cut、Unrated、Theatrical 等明确剪辑标记生成独立逻辑条目。
- 保留稳定的默认 source，并通过 MediaSourceId 允许客户端选择其他 source。

## 已完成

- [x] ParsedMovieFilename 提取 edition_name 和 quality_label。
- [x] 电影扫描将清晰度写入 source，将明确剪辑版写入独立 item。
- [x] Lux 目录和 Emby PlaybackInfo 返回版本标签。
- [x] 4K/1080p/Director's Cut fixture 与显式 source 选择测试。

## 边界

- 当前不凭相似标题、文件大小或编码猜测版本关系。
- 当前尚未从在线 provider ID 自动聚合；在线识别完成后应使用 provider ID 或管理员确认再扩展聚合。
- 剧集版本的文件名规则沿用现有 episode identity，后续需要单独的剧集版本 fixture 和策略。

## 验证

    cargo test --test scanner --test playback
    cargo clippy --all-targets --all-features -- -D warnings
