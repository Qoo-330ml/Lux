# LUX-157：扫描与实时元数据自动补全边界

## 目标

将手动完整扫描、实时增量索引和在线元数据补全明确分层，并为每个媒体库增加“实时新资源自动补全元数据”开关。

## 行为契约

- `realtimeWatchEnabled` 保留为兼容字段，服务端始终按开启处理，不提供关闭文件监听的能力。
- 新增 `realtimeMetadataAutoMatchEnabled`，默认 `false`，只控制实时增量扫描完成后的在线补全。
- 手动“扫描媒体库文件”不创建在线元数据任务；新媒体库/新根路径首次自动扫描仍按既有规格提交 `FILL_MISSING`。
- 实时自动补全只提交本次增量任务影响的、仍有可用媒体源的条目，不重新扫描整库。
- 在线补全使用 `FILL_MISSING`，尊重本地 NFO、已有图片、锁定字段和高置信度门槛。
- 扫描任务重启或重试时保留其是否为首次自动元数据扫描的意图。

## 验收

- [x] 手动完整扫描只产生扫描、本地元数据和探测事件，不产生 `METADATA_AUTO_MATCH_QUEUED`。
- [x] 新根路径首次扫描仍可以产生自动元数据任务。
- [x] 实时开关关闭时，新文件只进入增量索引，不产生元数据任务。
- [x] 实时开关开启且媒体库配置刮削器时，新文件索引完成后产生受影响条目的 `FILL_MISSING` 任务。
- [x] 实时自动任务不包含删除后不可用的条目，完整条目可被补全 worker 跳过。
- [x] 管理 API 和 Web 编辑界面可以读取、修改并持久化该开关。

## 预计修改范围

- Rust：媒体库模型、SQLite/PostgreSQL schema、扫描任务、实时监听、管理 API、集成测试。
- Web：管理媒体库 API 类型、创建/编辑表单和组件测试。
- 文档：`docs/LUX-DEVELOPMENT.md` 与本计划。

## 验证

```bash
cargo test --locked --test scanning_jobs
cargo test --locked --test watch
cargo test --locked --test libraries_api
cargo test --locked --test metadata_selection
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
pnpm --dir web test
pnpm --dir web build
```
