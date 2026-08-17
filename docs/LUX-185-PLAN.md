# LUX-185：Web 原生播放引擎与 HEVC 客户端兜底

## 目标

让 Lux Web 在浏览器原生支持的媒体上继续 DirectPlay；对浏览器不能原生解码、但具备必要 Web 能力的 HEVC
媒体，在客户端使用 WASM 解码和 H.264 编码后通过 MSE 播放。Lux 服务端不转码、不 Remux、不代理媒体内容。

## 技术边界

- `@hevcjs/core` 固定使用 MIT 许可版本；包内 WASM 解码器和 Worker 资产由 Lux Web 同源发布。
- `mp4box` 由 `@hevcjs/core` 运行时使用，许可为 BSD-3-Clause；随 Web 依赖记录，不复制或修改其源码。
- 客户端 fallback 支持 MP4/fMP4 HEVC 视频和 AAC 音频，并支持 MKV 中的 HEVC 视频与 AAC-LC 音频；MKV 的 DTS/Opus、Dolby Vision、DRM、PGS/ASS 渲染仍不在范围内。
- Safari 等支持 HEVC MSE 的浏览器优先走客户端 MKV → HEVC/AAC fMP4 封装路径；该路径只在浏览器端重封装样本，不解码、不降为 SDR，也不改变服务端的原始 Range 传输。
- H.264 编码由浏览器 `VideoEncoder` 完成；不满足 `VideoEncoder.isConfigSupported` 时不启用 fallback。
- WASM/转码工作在 Worker；UI 主线程只负责引擎状态、MSE 控制和播放进度。
- 4K 是能力目标，不是所有设备的实时性能承诺；必须以实际 `speedX`、丢帧和音画同步验收。

## 增量

1. 定义 `PlaybackEngine` 契约并抽取 `NativeVideoEngine`，确保现有 Web 播放回归通过。
2. 安装依赖、发布 Worker/WASM 资产，并实现客户端 MP4/fMP4 fallback 的输入与 MSE 输出。
3. 增加流式 Matroska/EBML 解封装，将 MKV 的 HEVC/AAC-LC 样本接入同一客户端 fallback，并在 `PlayerPage` 按媒体流和浏览器能力选择引擎。
4. 统一进度/错误/生命周期事件，用真实浏览器测试原生 H.264、原生 HEVC、MP4 fallback、MKV fallback、seek、停止和失败降级。

## 安全与资源约束

- Worker 只接收服务端生成的媒体 URL；不接受客户端磁盘路径。
- 不把完整 URL、令牌、Cookie 或用户媒体数据写入日志或持久化状态。
- 默认限制客户端缓冲窗口和并发转码实例；路由离开必须释放 Worker、MSE 和 Blob URL。
- `.strm` 外部 URL 不绕过 CORS；不借此新增 Lux 上游代理。

## 完成标准

- 现有 Web 测试和构建通过。
- 真实浏览器可证明至少一个 MP4/fMP4 和一个 MKV H.265 fallback 文件完成 metadata、播放、seek 和进度上报。
- 原生播放路径无回归，第三方客户端无行为变化。
- 兼容性文档记录浏览器、平台、媒体参数和客户端处理结果。

## 当前实现记录

本增量已完成播放引擎边界、客户端 HEVC fallback、流式 Matroska 解封装、MKV 的 HEVC/AAC-LC 输入、Worker/WASM 资产发布和 PlayerPage 选择接入。
fallback 的视频输出使用 H.264 fMP4 MSE；MP4/fMP4 沿用现有的音频处理。MKV 在支持 HEVC MSE 的浏览器中将
HEVC/AAC-LC 样本在 Worker 内封装为 fMP4，尽量保留 8/10-bit 视频；不支持 HEVC MSE 时才回退到 H.264 SDR
客户端编码路径。两条路径都保持服务端不转码、不 Remux、不代理媒体内容。播放、暂停、seek 和资源销毁通过
客户端事件同步。

MKV 路径当前明确支持 `V_MPEGH/ISO/HEVC` 视频和 `A_AAC/MPEG4/LC` AAC-LC 音频，支持常见 SimpleBlock
lacing 和 Annex-B/四字节长度前缀样本；其他音频编码会给出可诊断的 fallback 错误。浏览器 Worker 的真实初始化
回归已通过；本机当前没有可提交的真实 MKV 样本，因此 MKV 的完整播放、seek 和音画同步真实性能门仍待使用
用户提供的脱敏样本或专门准备的临时样本完成，不能据此宣称所有 MKV 或 4K MKV 均可实时播放。

MP4Box 生成的分段可能把负 composition offset 写成无符号 32 位值。客户端只在内存副本上把包含高位符号的
`trun` 标记为 signed，避免 B 帧时间戳溢出污染 MSE duration。

Safari HEVC MSE 路径使用原始 `hvcC` Box 写入 `hvc1` 样本描述，绕过 MP4Box 对 Matroska `CodecPrivate` 的
浏览器端解析路径；Worker 烟测已验证 `ready`、初始化片段、媒体片段和完成事件。该路径仍需使用真实用户
提供的 MKV 完成 Safari metadata、播放、seek、音画同步和 4K 性能验收。

已完成真实 4K 样本测试：Chrome 151/macOS arm64 对 3840×2160 HEVC Main + AAC 和 3840×2160 HEVC Main10
样本均完成 Worker/WASM 解码、H.264 编码、播放、seek 和 destroy；`43a7b8e6` 起 `setSource()` 在首个视频片段
可播后即返回，后续转码在后台完成。两者仍记录为低于实时，`PlayerPage` 会显示明确的降级提示，不虚报 4K
实时能力。`79035ba7` 修正了 4K H.264 level 探测，并用真实 `PlayerPage` 路径验证了 fallback 选择、首帧、进度
上报和低于实时提示。具体样本校验值、首段/完整耗时、帧回调、丢帧判断、漂移和 speedX 见
`docs/COMPATIBILITY.md` 与 `docs/PERFORMANCE.md`。
854×480 HEVC Main + AAC 烟测仍作为快速回归样本保留。
