# LUX-167：元数据对象快照写入

## 范围

为 `collections`、`genres`、`studios`、`tags` 提供共用的配置卷快照写入能力，文件位于对应对象目录：

    /config/metadata/<kind>/<bucket>/<display-name>-<provider>-<object-id>/
    └── <kind-singular>.json

已有合集刷新在数据库关系更新成功后写入 `collection.json`；快照是可重建缓存，数据库仍是合集关系和
查询事实来源。genres、studios、tags 本任务只提供写入能力，不伪造尚未存在的对象数据源。

## 验收

- [x] 对象快照包含 kind、展示名、provider、object ID，并支持简介和成员数摘要。
- [x] 快照写入使用配置卷 metadata 根目录，父级符号链接、越界路径和过大内容被拒绝。
- [x] 写入采用临时文件、同步、原子替换和临时文件清理。
- [x] 合集刷新会生成可重建的 `collection.json`，重复刷新可覆盖旧快照。
- [x] genres、studios、tags 复用同一对象存储边界，但不新增数据库关系或 API。

## 验证

    cargo test --locked --test metadata_objects --test collections
    cargo build --locked
    cargo test --locked --all-targets
    cargo fmt --all -- --check
    cargo clippy --locked --all-targets --all-features -- -D warnings
    uname -m
