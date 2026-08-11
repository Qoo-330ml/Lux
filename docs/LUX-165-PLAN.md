# LUX-165：媒体图片进入统一 metadata/library

## 范围

将 Lux 通过刮削器下载并由 item_images 登记的媒体图片写入：

    /config/metadata/library/<shard>/<item-id>/
    ├── poster.jpg
    ├── fanart.webp
    ├── logo.png
    └── thumb.jpg

媒体目录中的已有 NFO、海报和背景图继续被扫描并优先使用；本任务不迁移、删除或覆盖这些本地
资源。旧版本已经登记在媒体目录的图片继续可读、可删，新的配置卷图片由图片服务和 Emby 兼容
端点统一提供。

## 验收

- [x] 新下载的图片写入 metadata/library/<shard>/<item-id>/，数据库 item_images.local_path
      指向新文件。
- [x] Lux 和 Emby 图片端点可以读取 metadata/library 图片，并继续读取媒体目录本地图片。
- [x] 图片删除只允许删除媒体根目录内的本地图片或 metadata/library 内的 Lux 图片。
- [x] 缺失判断同时检查本地旁车图片和 metadata/library 图片；不会重复下载。
- [x] 配置目录父级符号链接、越界路径、损坏图片和原子写入有测试。
- [x] 自动媒体库封面可以读取两种位置的 poster。

## 兼容策略

- 读取顺序：数据库登记的当前路径；扫描得到的媒体目录本地图片仍按本地优先规则登记。
- 不对已有数据库执行不可逆路径改写；只在新下载或明确刷新时使用新路径。
- 不在用户请求路径扫描整个 metadata 目录，单个 item 只访问其稳定目录。

## 验证

    cargo build --locked
    cargo test --locked --all-targets
    cargo fmt --all -- --check
    cargo clippy --locked --all-targets --all-features -- -D warnings
    uname -m
