# LUX-173 实施计划：片头片尾章节标记存储

## 目标

为每个 `media_source` 建立片头片尾章节标记表，作为后续 Emby 输出和检测插件的服务器 DB
事实来源。当前任务只建立存储合同，不提前实现 LUX-174 至 LUX-176。

## 已确认合同

- 章节归属物理媒体源，不归属逻辑条目。
- 当前只允许 `INTRO_START`、`INTRO_END`、`CREDITS_START`；不允许普通 `CHAPTER`。
- 当前唯一章节来源是后续 `chapter_detector` 插件；每条记录必须有插件 ID 和 0 到 1 的置信度。
- 每个插件对同一媒体源的每种标记最多一条，起点和 `chapter_index` 必须非负。
- Lux 不主动读取容器章节，现有 ffprobe 参数、NFO/EDL 和媒体字节保持不变。
- 当前任务不写任何业务章节记录；LUX-175 负责校验并原子替换插件检测结果。

## 增量与精确文件

### 增量 1：标记合同和迁移

- `migrations/0055_media_chapters.sql`
- `migrations-postgres/0010_media_chapters.sql`
- `tests/storage.rs`
- `tests/postgres_database.rs`

先写迁移失败测试，再实现 `media_chapters` 的来源级外键、特殊枚举、范围和唯一性约束。

### 增量 2：回归与版本基线

- 与 schema version 强绑定的现有测试文件（仅更新预期版本）
- `tests/probe.rs`（仅运行现有回归，不修改探测合同）

验证数据库版本升级不改变本地媒体或 STRM 探测行为。

## 验证

- `cargo test --locked --test probe`
- `cargo test --locked --test storage`
- `cargo test --locked --test postgres_database -- --ignored`
- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`

## 明确不做

- 不读取或保存容器普通章节。
- 不返回 Emby `ChapterInfo`；属于 LUX-174。
- 不增加检测任务、插件类型或 Chromaprint；属于 LUX-175。
- 不修改媒体容器，不读写 NFO/EDL。
