# LUX-166：辅助元数据对象目录合同

## 范围

为后续合集、类型、工作室和标签资源建立统一的安全路径规则：

    /config/metadata/<kind>/<bucket>/<display-name>-<provider>-<object-id>/

其中 `<kind>` 为 `collections`、`genres`、`studios` 或 `tags`，`<bucket>` 为展示名的首个
字母或数字，用于避免单目录堆积；真实身份由 provider 和 object ID 组成。

本任务只提供路径工具和契约测试，不创建数据库表、不修改现有集合关系、不增加 API，也不执行
TMDb 自动合集、类型/工作室/标签索引或资源写入。

## 验收

- [x] 四类对象使用独立的 metadata 子目录。
- [x] 路径包含可读展示名、provider 和受校验的 object ID。
- [x] 展示名经过安全清洗，provider/object ID 拒绝空值、路径分隔符和控制性路径组件。
- [x] 路径契约测试覆盖中文名称、不同对象类型和越界输入。

## 验证

    cargo test --locked --test metadata_paths
    cargo build --locked
    cargo test --locked --all-targets
    cargo fmt --all -- --check
    cargo clippy --locked --all-targets --all-features -- -D warnings
    uname -m
