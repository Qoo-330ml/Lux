# MediaInfoKeeper 改写为 Lux 插件的设计与实现说明

> 状态：已选择插件路线；核心实现已落地，管理 Web 页面和计划调度仍待后续任务
>
> 日期：2026-08-05
>
> 本文记录 MediaInfoKeeper 到 Lux `media_probe` 插件的边界、协议和当前实现，不等同于把 Emby 插件直接移植进 Lux。

## 1. 结论

可以改写，采用“外部进程插件 + Lux 宿主任务”的拆分：

1. `org.lux.media-info` 是遵循 Lux Plugin SDK v1 的独立 `media_probe` 进程，负责调用 `ffprobe` 并返回受限结果。
2. Lux 核心负责媒体库选择、任务生命周期、URL 安全、并发、结果落库和 `*-mediainfo.json` 写回。

MediaInfoKeeper 的核心流程不是单纯的元数据转换，而是：选择媒体库、遍历媒体项、读取 `.strm` 指向的外部地址、调度并发 `ffprobe`、保存探测结果，并在需要时写入旁车文件。这些操作都属于 Lux 核心服务负责的资源边界。当前插件 SDK 的插件不能直接访问 Lux 数据库、媒体库根目录或内部任务对象，因此不适合直接承载这一流程。

因此把它定义为 Lux 的 **STRM 远程媒体探测插件能力**：

- Lux 核心负责任务、媒体库选择、路径和 URL 校验、并发控制、插件监督、结果落库和旁车文件写入。
- 插件只接收单个已通过宿主校验的 URL，不能访问 Lux SQLite、媒体根目录或内部任务对象。
- `.strm` 播放仍然是客户端直连；远程探测只在管理员显式创建的后台任务中发生。

## 2. 与当前 Lux 规范的冲突

当前 `docs/LUX-DEVELOPMENT.md` 明确规定：`.strm` 主要提供外部播放地址，扫描或后台探测不得主动读取其指向的远程资源；`.strm` 的技术信息只能从已有的 `*-mediainfo.json` 或 NFO 读取；`ffprobe` 只允许用于本地媒体文件。

因此，“主动对 `.strm` URL 执行 `ffprobe`”是产品边界变化。本次已通过新增 `media_probe` 插件类型和 `LUX-146` 任务纳入产品规范；探测仍不进入扫描请求、播放请求或 PlaybackInfo 请求路径。

未创建 STRM 探测任务时仍然保留原有行为：

- `.strm` 首个非空行作为外部播放地址。
- 播放流程不验证、不探测、不代理该地址。
- 已存在的旁车文件或 NFO 可以提供技术信息。
- 本地媒体仍由现有 Lux `MediaProbeService` 使用 `ffprobe` 探测。

管理员创建 STRM 探测任务后，Lux 才会按本说明调用 `org.lux.media-info`。

## 3. MediaInfoKeeper 功能到 Lux 的映射

| MediaInfoKeeper 功能 | Lux 改写方案 | 备注 |
| --- | --- | --- |
| 选择一个或多个媒体库 | 管理员提交媒体库 ID 列表 | 只允许选择当前账号可管理的媒体库，服务端再次校验 |
| 全库或最近项目扫描 | 作为持久化后台任务执行 | 不在 HTTP 请求中扫描，也不使用不可追踪的 fire-and-forget 任务 |
| 全局/单库并发数 | 复用每库 `probeConcurrency`，增加 STRM 远程探测全局上限 | 实际并发取两者的约束交集 |
| `.strm` 外部地址探测 | 新增显式开启的远程探测策略 | 默认关闭，且必须经过 URL 安全策略 |
| `ffprobe` 媒体信息 | 解析为 Lux 的 `media_sources` 和 `media_streams` | 不把原始 `ffprobe` JSON 直接作为公共数据模型 |
| `*-mediainfo.json` | 继续兼容读取；可选写回 | 写回必须使用受限、可兼容的旁车格式 |
| 失败重试/超时 | 任务级取消、重试、恢复和错误统计 | 单个 URL 失败不能中断整个媒体库任务 |
| 右键/计划任务 | 管理 API 触发；计划任务待 Lux 调度器完成后接入 | 不提前假设当前尚未完成的调度器能力 |

现有 Lux 已经具备一部分基础：媒体源和媒体流表、每库探测并发配置、旁车/NFO 读取、受输出大小和超时限制的 `ffprobe` runner，以及持久化扫描任务。改写重点是增加“远程 STRM 探测”的明确边界和任务生命周期，而不是复制 Emby 插件内部实现。

## 4. 方案：Lux 宿主任务 + 独立媒体探测插件

### 4.1 运行流程

```text
管理员选择媒体库
        │
        ▼
Lux API 校验权限、配置和库 ID
        │
        ▼
为每个媒体库创建 STRM_REMOTE_PROBE 任务
        │
        ▼
后台 worker 取出 STRM_URL 媒体源
        │
        ├─ 已有旁车/NFO 且未要求重探测：导入并跳过网络请求
        │
        └─ 需要探测：通过 URL 安全策略后执行 ffprobe
                │
                ▼
        解析受限结果 → 保存 media_sources/media_streams
                │
                ├─ 可选：原子写入同目录 mediainfo 旁车文件
                └─ 更新任务进度、错误和可重试状态
```

HTTP handler 只负责解析请求、权限校验和调用应用服务；库扫描、读取文件、网络请求、`ffprobe` 和 SQL 均在应用层、worker 层及 storage 层完成。

### 4.2 媒体库选择

建议提供一次性管理员操作：

```json
{
  "libraryIds": ["library-id-1", "library-id-2"],
  "includeReady": false,
  "writeSidecars": false
}
```

字段为提案，不是已冻结的 API：

- `libraryIds` 必填，不能为空，服务端限制最大数量。
- `includeReady` 控制是否重新探测已有成功结果；默认跳过。
- `writeSidecars` 控制是否写回旁车文件；建议默认关闭，避免未确认的文件系统副作用。

如果需要“全部媒体库”，应由 API 显式接受 `allLibraries: true`，并记录当次展开后的媒体库快照；不能把空数组同时解释为“全部媒体库”和“没有选择”。

### 4.3 并发模型

MediaInfoKeeper 使用全局队列和单项去重。Lux 应改为可恢复的持久化任务，并采用两级限流：

- **全局 STRM 远程探测上限**：防止多个媒体库同时操作时压垮网络、CPU 或外部服务。
- **每媒体库 `probeConcurrency`**：复用现有库配置，限制该库占用的 worker 数。

单项任务只有在同时取得全局和媒体库配额后才执行插件 RPC。当前实现的任务并发范围为 1–64，默认请求并发为 2，并取任务设置与媒体库 `probeConcurrency` 的较小值。

任务必须具备：

- 持久化进度和已处理游标。
- 取消后不再领取新项目，正在执行的 `ffprobe` 在超时或取消边界结束。
- 进程重启后可以恢复未完成项目。
- 同一媒体库不能重复创建冲突的活动任务。
- 同一媒体源去重，避免扫描任务和手动任务同时探测同一个 URL。

### 4.4 `ffprobe` 执行

复用 Lux 现有的安全 runner，但增加远程输入专用策略：

- 使用参数数组启动进程，不经过 shell。
- 采用固定的 `ffprobe` 参数集合，只读取 format、stream 等必要信息。
- 维持超时、stdout/stderr 上限，超限视为失败。
- 网络请求只在后台 worker 中执行。
- 不记录完整 URL、查询字符串、认证信息或 `ffprobe` 完整命令行。
- 不把原始探测 JSON 直接返回 API 或写入日志。

远程输入失败应归类为稳定的错误码，例如 `INVALID_URL`、`BLOCKED_ADDRESS`、`TIMEOUT`、`PROCESS_FAILED`、`INVALID_OUTPUT`，并将详细诊断限制在本地受控日志中。

### 4.5 结果保存

优先复用现有数据模型：

- `media_sources` 保存容器、大小、码率、时长、探测状态和错误摘要。
- `media_streams` 保存视频、音频和字幕流，以及受限的 details JSON。
- `probe_status` 继续使用 `PENDING`、`READY`、`FAILED`、`TIMEOUT` 等状态。
- 不新增 MediaInfoKeeper 专用数据库表，除非后续确认确实需要保留其额外字段。

如果需要区分“本地 ffprobe、旁车导入、STRM 远程 ffprobe”三种来源，可以在单独的数据库迁移中增加 `probe_origin`；这属于公共模型变更，必须先更新规范并从空数据库验证迁移链，不能在实现中顺手加入。

## 5. 旁车文件方案

### 5.1 命名和位置

为了兼容 MediaInfoKeeper，默认采用同目录、去掉媒体扩展名后追加 `-mediainfo.json` 的命名：

```text
Movie.strm
Movie-mediainfo.json
```

旁车路径必须由 `.strm` 的规范化本地路径计算，且位于媒体库根目录内。外部 URL 只能作为探测输入，不能被拼接成文件路径，也不能写入旁车文件。

### 5.2 读取兼容

现有 Lux 已经能够读取 MediaInfoKeeper 风格的数组/对象 JSON，并映射到 Lux 的媒体源和媒体流模型。改写应保留以下兼容性：

- 接受 MediaInfoKeeper 的数组根结构。
- 只读取 Lux 支持的字段，未知字段忽略。
- 旁车损坏时记录可诊断错误并回退到 NFO 或远程探测策略。
- 旁车读取不能让 `.strm` 播放请求触发网络请求。

### 5.3 写回策略

MediaInfoKeeper 的行为是探测后写入旁车文件；Lux 建议把这个行为设为显式开关，而不是默认副作用。

启用后：

- 只写入 Lux 定义的兼容子集，不写原始 `ffprobe` JSON。
- 不写入完整外部 URL、认证 token、内部路径、数据库 ID 或宿主敏感信息。
- 使用临时文件、`fsync`（按平台能力）和原子 rename，避免进程中断留下半个 JSON。
- 只允许写入媒体库根目录下、由媒体项路径推导出的目标文件。
- 文件已被用户修改时，需要由配置决定“覆盖、跳过或保留并仅更新数据库”；建议默认跳过并在任务结果中统计。

## 6. URL 安全策略

一旦 Lux 主动访问 `.strm` URL，原本“只把 URL 交给客户端播放”的安全边界就改变为服务器端出站请求，必须有独立的 SSRF 和凭据保护策略。

当前实现：

- 只接受 `http` 和 `https`，拒绝 `file`、`data`、`ftp`、Unix socket 等非预期输入。
- 拒绝 URL 用户名/密码、fragment、localhost、`.localhost`、云实例元数据主机，以及字面量回环、私网、链路本地、未指定、多播和共享地址。
- 宿主和媒体探测插件都会执行策略校验；策略失败只产生安全摘要，不返回完整 URL。
- 不在日志、错误消息、任务事件或插件 RPC 中暴露完整 URL。
- 不允许将 URL 中的用户名、密码、token 复制到旁车或返回给 Web 客户端。

当前仍有两个生产化风险需要后续处理：DNS 名称解析后的私网地址/rebinding，以及 ffprobe 内部重定向的逐跳策略。若要支持局域网 NAS/AList，应增加管理员显式 allowlist，而不是直接放宽默认拒绝策略。

## 7. API 和管理界面提案

当前管理员操作端点为：

```text
POST /api/v1/admin/strm-probe-jobs
```

响应使用 `202 Accepted`，返回任务 ID 或按媒体库拆分的任务 ID；不返回 URL。任务使用独立 `strm_probe_jobs` 表，提供分页列表、详情、取消和重试接口；列表仍设置服务端分页上限。

建议在管理界面展示：

- 可选择的媒体库。
- 当前全局和各库并发限制。
- 待处理、成功、跳过、失败、超时数量。
- 失败错误码和是否可重试。
- 是否导入已有旁车、是否写回旁车。
- 取消、重试和重新探测已有成功项。

不建议在普通媒体详情或播放请求中增加“立即远程探测”按钮；这会把外部网络和 `ffprobe` 带入用户请求路径，违反当前服务边界。

计划任务可以在 Lux 调度器完成后接入，复用同一应用服务。调度器尚未完成前，第一版只实现管理员手动触发，避免把未来能力写成当前承诺。

## 8. 外部插件边界与宿主职责

当前 Plugin SDK 的边界是独立进程、JSON-RPC、宿主提供受限调用；本次已经增加以下协议能力：

```text
manifest.type = "media_probe"
capability   = "media.probe"
permission   = "network: media-source"
RPC          = media.probe
```

插件调用是单项 RPC，而不是让插件拥有任务控制权。宿主负责：

- 安全地把远程地址交给插件，并拒绝不符合协议/地址策略的输入。
- 通过独立进程、超时和输出大小上限限制插件资源。
- 持久化任务并在服务重启后恢复 PENDING/RUNNING 任务。
- 将插件结果限制为 `media_sources`、`media_streams` 可接受的字段。
- 按管理员开关原子写入兼容旁车文件。

插件不能自行选择媒体库、遍历文件、写旁车或改变 Lux 数据库；插件进程退出、超时或返回异常结果只影响对应源/任务。

## 9. 分阶段实现计划

正式任务编号使用 `LUX-146`；Web 管理页面和计划任务仍另行拆分。

### 阶段 0：产品边界确认（已完成）

- 确认是否允许服务器主动探测 `.strm` 远程地址。
- 确认第一版采用 Lux 宿主任务加外部 `media_probe` 插件。
- 确认是否允许写回 `*-mediainfo.json`，以及默认开关。
- 确认局域网地址和带凭据 URL 的处理策略。
- 更新 `docs/LUX-DEVELOPMENT.md`，必要时新增架构 ADR。

### 阶段 1：探测策略和结果模型（已完成）

- 为远程 URL 增加独立的安全策略和可测试的插件 `ffprobe` 输入适配器。
- 复用现有旁车解析和媒体流映射。
- 完成超时、输出上限、错误码和 URL 脱敏测试。
- 不添加 API 和 Web 页面。

### 阶段 2：持久化任务和并发调度（已完成）

- 增加独立 `strm_probe_jobs` 任务模型。
- 实现多媒体库选择、每库并发、全局并发、去重、恢复、取消和重试。
- 让任务重启后可以从游标继续，不重复处理已完成媒体源。
- 从空数据库运行迁移并验证旧数据库升级。

### 阶段 3：管理 API（核心 API 已完成）

- 增加管理员触发接口和请求校验。
- 复用分页任务查询、取消、重试和事件接口。
- 错误响应只返回错误码和安全摘要，不返回 URL。

### 阶段 4：旁车写回（核心实现已完成）

- 增加兼容 MediaInfoKeeper 的受限序列化器。
- 实现根目录校验、原子写入、冲突策略和写入统计。
- 使用真实文件系统测试临时目录，不使用用户媒体库和真实 URL。

### 阶段 5：Web 管理界面

- 增加媒体库多选、并发配置、任务进度和错误处理。
- 只调用管理 API，不在浏览器中执行或探测外部媒体 URL。
- 完成 Web 单元测试和构建检查。

### 阶段 6：可替换探测器扩展

后续可以增加其他 `media_probe` 实现，但不能反向改变核心任务、数据安全和权限边界。

## 10. 可能涉及的文件范围

以下是设计阶段识别出的现有接入点，不代表本提案已经授权修改：

- `src/application/probe.rs`：旁车导入、结果映射和兼容旁车写入。
- `src/application/strm_probe.rs`：STRM 任务、并发、取消、恢复和插件结果持久化。
- `src/application/strm_probe_policy.rs`：远程 URL 协议和 SSRF 基础策略。
- `src/bin/lux-plugin-media-info.rs`：独立 `ffprobe` 插件进程。
- `src/application/plugin_protocol.rs`、`src/application/plugin_runtime.rs`：媒体插件 manifest、RPC DTO 和隔离调用。
- `src/application/scanner.rs`：扫描完成后的后台探测衔接。
- `src/api/mod.rs`：管理 API 和任务服务注入。
- `src/storage/`：媒体源、媒体流和任务查询/保存。
- `migrations/`：如果新增任务类型或探测来源字段，需要新增迁移。
- `docs/API.md`：API 和 PlaybackInfo 约定确认后再更新。
- `docs/PLUGIN-SDK.md`：`media_probe` manifest 和 RPC 约定。

不得直接移植 MediaInfoKeeper 的 Emby Harmony patch、Emby DLL 调用或静态全局 runner。Lux 应通过自身的应用服务、storage 和有界 worker 完成同等能力。

## 11. 验收标准

在产品边界获批并实现后，至少满足：

- 管理员可以明确选择一个或多个媒体库，且只处理这些库中的 `.strm` 媒体源。
- 未选中的媒体库不产生任务或远程请求。
- 实际并发不会超过全局上限和对应媒体库的 `probeConcurrency`。
- 相同媒体源不会被重复探测；任务支持取消、重试和进程重启恢复。
- 成功结果正确写入 `media_sources` 和 `media_streams`，PlaybackInfo 可以复用这些结果。
- 已有 MediaInfoKeeper 风格旁车仍可被读取；损坏旁车不会阻塞整个任务。
- 启用写回时，旁车文件使用兼容格式、原子写入，并且不包含完整 URL、token、内部 ID 或原始 `ffprobe` JSON。
- URL 校验、重定向、超时、异常输出和进程失败均有稳定错误码，且日志已脱敏。
- 任何用户请求都不会同步扫描媒体库、访问远程 `.strm` 地址或运行 `ffprobe`；只有管理员显式创建的后台任务可以触发媒体探测插件。
- `.strm` 原有直连播放行为不改变，不增加转码、代理或流媒体缓存。
- 从空数据库执行全部迁移成功，升级现有数据库不丢失媒体源和媒体流数据。

## 12. 后续产品决策

1. 是否增加管理员显式 allowlist，以支持 NAS/AList 等局域网地址？
2. 是否为带 query token 的 URL 增加受控凭据策略；当前实现只拒绝 URL 用户名/密码，不会把 query 原样写入日志或旁车。
3. 是否在调度器完成后接入计划任务；当前只提供管理员手动触发。
4. 是否增加 ffprobe 重定向逐跳校验和 DNS rebinding 防护；当前实现已明确将其作为生产化前置风险。

## 13. 参考资料

### MediaInfoKeeper

- [项目主页](https://github.com/honue/MediaInfoKeeper)
- [媒体信息任务的历史实现](https://github.com/honue/MediaInfoKeeper/blob/5029da1/ScheduledTask/ExtractRecentMediaInfoTask.cs)
- [当前并发队列实现](https://github.com/honue/MediaInfoKeeper/blob/master/Services/MediaInfoRunner.cs)
- [媒体信息服务](https://github.com/honue/MediaInfoKeeper/blob/master/Services/MediaInfoService.cs)
- [`ffprobe`/`ffmpeg` 放行逻辑](https://github.com/honue/MediaInfoKeeper/blob/master/Patch/MediaInfo/FfProcessGuard.cs)
- [旁车文档模型](https://github.com/honue/MediaInfoKeeper/blob/master/Store/MediaInfoDocument.cs)
- [媒体信息保存逻辑](https://github.com/honue/MediaInfoKeeper/blob/master/Store/MediaSourceInfoStore.cs)

### Lux

- [Lux 产品规范](LUX-DEVELOPMENT.md)
- [Lux Plugin SDK](PLUGIN-SDK.md)
- [本地媒体探测计划](LUX-033-PLAN.md)
- [媒体库探测并发配置计划](LUX-044-PLAN.md)
- [STRM 播放边界计划](LUX-072-PLAN.md)
