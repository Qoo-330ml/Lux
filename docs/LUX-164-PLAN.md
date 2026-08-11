# LUX-164：统一元数据资源目录与人物布局

## 范围

建立 `/config/metadata` 的路径合同和安全写入基础，并将人物缓存迁移到按首字符、名称、Provider
和 Provider ID 分组的对象目录。旧 `/config/people/items` 与 `/config/people/profiles` 继续只读兼容。

本任务不新增人物数据库关系、不改变媒体目录 NFO 写回、不实现 genres/studios/tags API；这些内容
在后续任务中按垂直切片实施。

## 验收

- [x] 人物头像和 `person.nfo` 写入 `/config/metadata/people/<bucket>/<name>-<provider>-<id>/`。
- [x] 人物条目关系快照写入 `/config/metadata/library/<shard>/<item-id>/people.json`。
- [x] 旧人物目录仍可读取，升级不会要求手工搬运或删除文件。
- [x] 路径清洗、稳定分片、符号链接拒绝和原子写入有自动化测试。
- [x] 不在请求路径扫描整个 metadata 目录，不记录外部图片完整 URL。

## 预计文件

- `docs/LUX-DEVELOPMENT.md`
- `docs/decisions/014-metadata-storage-layout.md`
- `src/application/metadata_paths.rs`
- `src/application/mod.rs`
- `src/application/people.rs`

## 验证

```bash
cargo test --locked --lib metadata_paths
cargo test --locked --test metadata_selection
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
uname -m
```

## 明确不做

- 不迁移或删除媒体目录中的 NFO、海报和背景图。
- 不新增 genres、studios、tags、views、livetv 或音乐库对象。
- 不新增人物详情公共 API，不改变现有 `/api/v1/people/{id}/image` 路由。
