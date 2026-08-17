# ADR-019：客户端 MKV HEVC 到 fMP4 的封装

## 状态

已接受

## 日期

2026-08-17

## 背景

Safari 等浏览器可能具备 HEVC 解码能力，却不能把 MKV 直接交给 HTML `video` 或 MSE。Lux 又不能在
服务端转码、Remux 或代理媒体内容。对于包含 HEVC 视频和 AAC-LC/AC-3/E-AC-3 音频的 MKV，需要一个不改变视频编码的
浏览器端路径。

## 决定

- 在 Web Worker 内流式解封装 MKV，将 HEVC 样本转换为 fMP4 所需的 length-prefixed 形式，并将 AAC-LC、AC-3 或
  E-AC-3 样本写入同一组 A/V fMP4 片段；AC-3 的 `dac3` 和 E-AC-3 的 `dec3` 配置从首个音频帧解析。
- HEVC 配置以原始 `hvcC` Box 写入 `hvc1` 样本描述，避免 MP4Box 在浏览器构建中再次解析 Matroska
  `CodecPrivate`。
- 浏览器同时支持组合 HEVC+对应音频 MSE 时才为 AC-3/E-AC-3 选择该路径；不支持时继续使用已有的客户端 HEVC 解码、
  H.264 SDR 编码路径或给出不可播放提示。
- 多音频 MKV 按 AAC-LC、AC-3、E-AC-3 的顺序选择可封装轨；DTS 不伪造为 Web 音频格式。DTS-only 资源在 Web
  端提示使用支持 DTS 的原生客户端，有 AAC-LC/AC-3/E-AC-3 备用轨时自动跳过 DTS。
- 服务端始终只提供原始鉴权 Range 数据，不执行转码、Remux 或代理；客户端路径不接受本地文件路径或外部
  未授权 URL。

## 未选择的方案

### 服务端转码或 Remux

兼容性更统一，但违反 Lux 的资源和部署边界，并会把 4K 视频处理压力转移到 NAS。

### 先把 HEVC 解码再编码为 H.264

可以覆盖不支持 HEVC MSE 的浏览器，但会增加 4K/10-bit 的 CPU/GPU 压力并降为 SDR，因此只作为能力
不足时的 fallback。

### 让 Safari 直接播放 MKV

Safari 的容器支持不足，不能依赖 `canPlayType` 解决解封装问题。

## 后果

- 支持 HEVC MSE 的设备可以保留原始 HEVC 码流，理论上保留 4K、10-bit 和 HDR10；Dolby Vision 仍不在承诺范围内。
- MKV 仍只承诺 HEVC + AAC-LC/AC-3/E-AC-3；DTS、Opus、PGS/ASS 和复杂字幕轨需要其他路径或兼容客户端。
- 片段时间轴、seek、音画同步和实时性能必须在真实浏览器与真实 MKV 样本上单独验收。
