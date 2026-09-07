# ADR-042：远程 STRM Chrome 扩展全媒体引擎

## 状态

提议；部分取代 ADR-040 第 5 项和 ADR-041 的“扩展只负责字幕”边界。LUX-245 完成阶段门前不宣称音视频解码已可用。

## 日期

2026-09-07

## 背景

当前扩展只在 Service Worker 中读取远程 Matroska 的文本字幕，原生 `<video>` 继续处理音视频。Chrome 对远程文件中的 E-AC-3、DTS、
部分 HEVC/AV1 组合没有统一解码能力，导致视频可能能显示但没有声音。用户希望保持媒体字节从远端直接进入浏览器，同时扩大浏览器端的
音视频兼容范围。

## 决定

1. 扩展可以增加独立的远程媒体引擎，但必须使用版本化媒体会话协议；Service Worker 只负责受限的远程 Range 读取和生命周期协调，解封装与
   解码放在可长期运行的 Extension Worker/页面上下文，不能依赖 Service Worker 持有 DOM、MediaSource 或 AudioContext。
2. 同一媒体会话只能有一个逻辑 Range 读取器。页面选择扩展引擎后，音频、视频和字幕都由该会话提供；不得再并行创建原生远程媒体连接、Lux
   字幕请求或第二条 Range Relay。扩展读取的字节不上传到 Lux。
3. 原生 MSE/WebCodecs 可消费的 codec 允许直通；浏览器明确不支持的 codec 只能由经过许可证审计的 WASM/WebCodecs 解码器处理。`isTypeSupported`
   或 `canPlayType` 单独返回 true 不足以声明支持，必须有真实夹具播放和同步证据。
4. 远端必须提供有效的 `206 + Content-Range`，且长度、ETag/资源代次和索引范围稳定。服务器返回 200、隐藏 Range、缺少 Cues/SeekHead 或
   资源变化时直接报告失败；不得为了“兼容”而整文件下载大型媒体。
5. 扩展不可用或媒体引擎失败时，若原生组合仍可播放则保持既有原生路径；否则显示具体的 codec/Range/索引错误。不会把失败静默变成 Lux
   服务端转码，也不会伪造音频可用。

## 未决依赖

- 需要选择能在 MV3/Worker 中运行、支持 Matroska 随机读取和目标音频/视频 codec 的 WASM 解码实现，并完成许可证、包体、内存和性能审计。
- E-AC-3/DTS 的音频输出需要 AudioWorklet/WebAudio 时间轴；视频软件解码需要 VideoFrame/WebCodecs 或受控 Canvas 输出。两者必须共享 seek、
  pause、playbackRate 和 drift 校正协议。
- 当前实测资源未提供有效 Range；在远端源修复前，只能验证失败诊断，不能验收全媒体扩展。

## 后果

- 成功后，远程扩展可在不经过 Lux 媒体服务器的情况下覆盖更多音视频 codec。
- 扩展体积、CPU、内存和电池开销明显增加；旧设备可能只能使用原生路径或显示不支持。
- 当前字幕扩展协议不能直接兼容全媒体协议，必须增加 session kind、能力报告、帧/PCM 时间戳和错误类别。

## 验证

详见 LUX-245：Range/索引边界、媒体会话生命周期、音画同步、seek/暂停、真实 Chrome 播放、扩展 ZIP 和许可证审计。
