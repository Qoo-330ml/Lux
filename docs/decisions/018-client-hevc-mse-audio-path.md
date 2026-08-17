# ADR-018: 客户端 HEVC fallback 的音频与时间轴处理

## Status

Accepted

## Date

2026-08-17

## Context

Lux Web 不能依赖服务端转码、Remux 或媒体代理。浏览器不支持 HEVC 时，需要在客户端用 WASM 解码、
浏览器 VideoEncoder 编码 H.264，再通过 MSE 播放。MP4/fMP4 输入还要保留 AAC 音频，并正确处理带 B 帧的
composition timestamp。

真实 Chromium 烟测发现：

- 部分运行环境对同一个 MediaSource 的第二个 audio SourceBuffer 返回 `QuotaExceededError`，即使视频和音频
  codec 都分别受支持。
- MP4Box 生成的某些 `trun` 片段将负 composition offset 表示成 unsigned 32-bit 值；解封装后会产生约
  2^32 时间单位的异常 PTS，导致 MSE duration 溢出。

## Decision

- 视频使用一个 H.264 MediaSource/SourceBuffer。
- AAC 使用隐藏的 audio 元素和另一个 MediaSource/SourceBuffer；播放器监听 video 的 play、pause、seeking 事件
  同步音频时间和生命周期。
- 交给 HEVC Worker 前，在客户端复制并检查 fMP4 `moof/traf/trun`；当 version 0 的 composition offset 具有
  高位符号时，只修改内存副本的 `trun` version 为 1，使其按 signed offset 解读。
- 读取源轨时长并尝试设置两个 MediaSource 的 duration；浏览器拒绝时保留 MSE 自己推导的 duration，不阻断播放。

## Alternatives Considered

### 服务端 Remux 或转码

可以统一音视频轨和时间轴，但违反 Lux 首版不转码、不 Remux、不代理媒体内容的边界，并增加 NAS CPU 和临时
任务管理成本。

### 同一个 MediaSource 添加音频 SourceBuffer

结构更简单，但真实浏览器存在 SourceBuffer 数量限制；失败会直接让 HEVC fallback 无法播放。

### 依赖浏览器原生 HEVC

原生路径仍优先使用，但无法覆盖不支持 HEVC 的浏览器，不能作为 fallback 方案。

## Consequences

- fallback 需要额外维护一个隐藏 audio 元素，并在销毁时移除监听器、MediaSource 和 Blob URL。
- 音视频同步由客户端事件驱动；未来若加入复杂 seek 或更长缓冲，需要补充 drift 监测和清理策略。
- 4K 是否实时播放仍取决于设备的 WASM 解码和 VideoEncoder 吞吐，必须用实际 4K 样本记录 speedX、丢帧和同步结果。
