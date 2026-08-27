# LUX-202：元数据资源默认写回媒体目录，可选 metadata 镜像

## 目标

将 Lux 通过刮削器生成或回写的 NFO、海报、季图和集图默认写入对应媒体目录的标准旁车位置；管理员
可在全局图片策略或媒体库自定义覆盖中开启 `images.writeToMetadata`，开启后在
`/config/metadata/library/<shard>/<item-id>/` 额外保存同一份 NFO 和图片。

媒体目录中的 NFO 和图片是主文件；图片继续登记到 `item_images.local_path`。历史只存在于
`metadata/library` 的资源不自动迁移；旧版策略 JSON 缺少新字段时按关闭处理。

## 验收

- [x] 新下载图片默认写入媒体目录，集图和季图使用现有标准命名规则。
- [x] 新生成或回写的电影、剧集、季、集 NFO 默认写入对应媒体目录旁车。
- [x] 开启 `writeToMetadata` 后 NFO 和图片同时写入媒体目录和 metadata/library，图片只下载一次。
- [x] 全局策略与媒体库自定义覆盖均能配置该开关；未配置的旧策略保持关闭。
- [x] NFO 和图片 API、缺失判断、刷新和删除不泄漏路径，也不破坏已有本地资源优先级。
- [x] Rust 与 Web 相关质量检查通过。

## 验证

```text
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
uname -m
```
