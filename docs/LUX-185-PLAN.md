# LUX-185：Web 原生播放引擎与 HEVC 客户端兜底

## 目标

让 Lux Web 在浏览器原生支持的媒体上继续 DirectPlay；对浏览器不能原生解码、但具备必要 Web 能力的 HEVC
媒体，在客户端使用 WASM 解码和 H.264 编码后通过 MSE 播放。Lux 服务端不转码、不 Remux、不代理媒体内容。

## 技术边界

- `@hevcjs/core` 固定使用 MIT 许可版本；包内 WASM 解码器和 Worker 资产由 Lux Web 同源发布。
- `mp4box` 由 `@hevcjs/core` 运行时使用，许可为 BSD-3-Clause；随 Web 依赖记录，不复制或修改其源码。
- 客户端 fallback 首先支持 MP4/fMP4 HEVC 视频和 AAC 音频；MKV、Dolby Vision、DRM、PGS/ASS 渲染另行处理。
- H.264 编码由浏览器 `VideoEncoder` 完成；不满足 `VideoEncoder.isConfigSupported` 时不启用 fallback。
- WASM/转码工作在 Worker；UI 主线程只负责引擎状态、MSE 控制和播放进度。
- 4K 是能力目标，不是所有设备的实时性能承诺；必须以实际 `speedX`、丢帧和音画同步验收。

## 增量

1. 定义 `PlaybackEngine` 契约并抽取 `NativeVideoEngine`，确保现有 Web 播放回归通过。
2. 安装依赖、发布 Worker/WASM 资产，并实现客户端 MP4/fMP4 fallback 的输入与 MSE 输出。
3. 在 `PlayerPage` 按媒体流和浏览器能力选择引擎，统一进度/错误/生命周期事件。
4. 用真实浏览器测试原生 H.264、原生 HEVC、fallback HEVC、seek、停止和失败降级。

## 安全与资源约束

- Worker 只接收服务端生成的媒体 URL；不接受客户端磁盘路径。
- 不把完整 URL、令牌、Cookie 或用户媒体数据写入日志或持久化状态。
- 默认限制客户端缓冲窗口和并发转码实例；路由离开必须释放 Worker、MSE 和 Blob URL。
- `.strm` 外部 URL 不绕过 CORS；不借此新增 Lux 上游代理。

## 完成标准

- 现有 Web 测试和构建通过。
- 真实浏览器可证明至少一个 H.265 fallback 文件完成 metadata、播放、seek 和进度上报。
- 原生播放路径无回归，第三方客户端无行为变化。
- 兼容性文档记录浏览器、平台、媒体参数和客户端处理结果。

## 当前实现记录

本增量已完成播放引擎边界、客户端 HEVC fallback、Worker/WASM 资产发布和 PlayerPage 选择接入。
fallback 的视频输出使用 H.264 MSE；AAC 音频使用独立的隐藏 audio 元素和独立 MediaSource，避免部分浏览器
限制同一个 MediaSource 的 SourceBuffer 数量，同时保持服务端不转码、不 Remux、不代理媒体内容。播放、暂停、
seek 和资源销毁通过客户端事件同步。

MP4Box 生成的分段可能把负 composition offset 写成无符号 32 位值。客户端只在内存副本上把包含高位符号的
`trun` 标记为 signed，避免 B 帧时间戳溢出污染 MSE duration。

已完成真实 4K 样本测试：Chrome 151/macOS arm64 对 3840×2160 HEVC Main + AAC 和 3840×2160 HEVC Main10
样本均完成 Worker/WASM 解码、H.264 编码、播放、seek 和 destroy；两者均记录为低于实时，`PlayerPage` 会显示
明确的降级提示，不虚报 4K 实时能力。具体样本校验值、帧数、丢帧、漂移和 speedX 见 `docs/COMPATIBILITY.md`。
854×480 HEVC Main + AAC 烟测仍作为快速回归样本保留。
