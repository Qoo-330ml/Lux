# LUX-173 实施计划：媒体章节存储与容器章节探测

## 目标

为每个 `media_source` 持久化普通容器章节，并把章节读取限制在后台 ffprobe 流程。当前任务只建立
后续 Emby 输出和片头片尾检测所需的可靠基础，不提前实现 LUX-174 至 LUX-176。

## 已确认合同

- 章节归属物理媒体源，不归属逻辑条目。
- 当前任务只写入 `marker_type = CHAPTER`、`origin = EMBEDDED`。
- ffprobe 输出是不可信输入：最多 10,000 章，起点必须非负，标题去除首尾空白后最多 512 字节。
- 同一探测结果按起点与原始顺序稳定排序并去重，保存时重编连续 `chapter_index`。
- 成功探测在保存媒体信息的同一事务中替换 EMBEDDED 章节；失败不触碰已有成功章节。
- 文件内容变化使所有来源章节失效，避免旧检测标记继续作用于新媒体字节。

## 增量与精确文件

### 增量 1：章节合同和迁移

- `migrations/0055_media_chapters.sql`
- `migrations-postgres/0010_media_chapters.sql`
- `src/storage/mod.rs`
- `tests/probe.rs`

先写迁移和存储失败测试，再实现 `media_chapters`、批量替换和读取原语。

### 增量 2：ffprobe 解析

- `src/application/probe.rs`
- `tests/probe.rs`

先扩展 fixture 断言失败，再加入 `-show_chapters`、受限解析、排序、去重和标题校验。

### 增量 3：后台探测原子保存与失效

- `src/application/probe.rs`
- `src/storage/mod.rs`
- `tests/probe.rs`
- 与 schema version 强绑定的现有测试文件（仅更新预期版本）

把章节与媒体源/流一起提交，并覆盖成功、失败、文件变化和保留非 EMBEDDED 来源。

## 验证

- `cargo test --locked --test probe`
- `cargo build --locked`
- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `uname -m`

## 明确不做

- 不返回 Emby `ChapterInfo`；属于 LUX-174。
- 不增加检测任务、插件类型或 Chromaprint；属于 LUX-175。
- 不修改媒体容器，不读写 NFO/EDL。
