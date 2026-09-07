# ADR-038：远程 STRM 原生播放默认与显式字幕单管线

## 状态

已接受；取代 ADR-037 的 native-only 结论，并恢复 ADR-035/036 中已经实现的显式远程字幕管线。

## 日期

2026-09-07

## 背景

当前远程 HTTP(S) STRM 的原生 `<video>` 可以播放音视频，但 Chrome 等浏览器并不保证把 MKV 内嵌字幕暴露为
`video.textTracks`。因此“只消费 native TextTrack”会让 API 已经列出的 SRT/ASS/SSA 轨道一直显示“暂不支持”，如用户实际截图所示。

同时，远程媒体不能在默认播放时交给 JavaScript：Range/CORS/MSE/codec 任一条件失败都会破坏原本可用的 Direct Play。

## 决定

1. 远程 HTTP(S) STRM 默认仍使用原生 `<video>`，优先使用播放计划的 `proxyUrl`，代理失败才沿用签名 Lux Direct URL。未选择字幕时不
   启动 Worker、Range、MSE 或客户端 MKV 引擎。
2. 当用户明确选择远程 Matroska/WebM 的 SRT、ASS 或 SSA 内嵌轨时，播放器才切换到现有 `ClientMkvEngine`。它通过当前播放会话
   签名的同源 `rangeUrl` 顺序读取同一媒体，Worker 同时解封装音视频和字幕；音视频 remux 到 MSE，字幕作为 Lux cue/原生 TextTrack
   交给覆盖层。不会请求外部 CDN 的 JavaScript URL，不创建字幕专用连接，不预先抽取或生成外挂文件。
3. 选择字幕只改变客户端引擎，不创建新的播放会话，不调用停止接口，不改变媒体源、tier 或 ACL。切换时保留当前播放位置和播放/暂停状态。
4. Range、索引、codec、MSE、Worker 或字幕解析失败时，只清除字幕选择并恢复同一播放计划的原生视频；不得进入服务端 HLS，也不得显示
   “播放器引擎失败”。
5. URL 型 HTTP(S) 以外的 STRM 不进入该管线；本地 Matroska 仍使用原有客户端 fallback 和 source-scoped 字幕合同。

## 后果

- 远程音视频继续拥有修改字幕前的原生播放兼容性。
- 浏览器未暴露 native TextTrack 时，用户仍可主动选择远程内嵌文本字幕；播放会短暂进入客户端 MSE 管线，而不是把字幕错误地标记为不存在。
- 显式字幕模式依赖同源 Range Relay、有效 Matroska 轨道和浏览器 MSE codec 支持。失败只影响字幕，不会终止原生视频。
- 该管线必须继续保持单一逻辑媒体读取器和严格资源上限。未来的 seek/Cues 优化应在该管线内部完成，不得恢复“默认所有远程 MKV 走 JS”。

## 验证

- 无字幕选择时没有 Range 请求，远程 `<video>` 使用原生代理/签名 URL。
- 选择字幕时读取播放会话 `rangeUrl`，不读取外部 URL；SRT/ASS/SSA 轨道进入字幕控制器。
- Range/MSE 失败时字幕被撤销、同一媒体计划恢复，播放会话不停止或重建。
