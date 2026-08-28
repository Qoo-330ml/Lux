# ADR-031：章节插件显式声明媒体源类型

## Status

Accepted

## Date

2026-08-28

## Context

Lux 的章节来源可以通过不同 RPC 合同工作：指纹检测宿主调用 FFmpeg 读取本地媒体，在线查询插件只接收
已保存的外部 ID、季号、集号和时长。最初宿主根据 `chapters.lookup` 或 `chapters.detect` 间接推断
`LOCAL_FILE`/`STRM_URL` 支持范围。这个推断没有区分插件身份，但仍然把媒体源策略绑定到了当前两种实现，
未来新增章节插件时无法表达自己的输入范围。

## Decision

`chapter_detector` manifest 必须声明 `supportedMediaSourceKinds`，当前允许的值是 `LOCAL_FILE` 和
`STRM_URL`。Lux 只使用该声明筛选候选媒体源，并在管理 API 中原样公开；插件 ID 不参与媒体源能力判断。

章节 capability 仍然只决定 RPC 合同：`chapters.lookup` 使用元数据查询合同，`chapters.detect` 使用
宿主生成的 Chromaprint 指纹合同。为避免调用路径歧义，一个章节插件必须且只能声明其中一个 capability；
当前 `chapters.detect` 合同只能声明 `LOCAL_FILE`，直到新增能安全处理其他媒体源的检测合同。

## Consequences

- TheIntroDB 可以通过 manifest 声明本地文件和 `.strm` 都可查询，不需要 Lux 认识其插件 ID。
- 新的在线或其他章节插件可以声明不同的媒体源组合；宿主候选查询和待处理任务会遵守该声明。
- 现有章节插件包必须携带该 manifest 字段；未声明或声明未知类型的包会被拒绝。
- 新增媒体源类型时，需要同时扩展 manifest 校验、存储候选查询和对应 RPC 合同的安全边界。
