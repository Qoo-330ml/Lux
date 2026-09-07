# ADR-040：远程 STRM Chrome 扩展字幕桥接

## 状态

已接受；部分取代 ADR-039 的“字幕读取必须使用 Lux 签名 Range Relay”决定。

## 日期

2026-09-07

## 背景

远程 HTTP(S) STRM 在修改字幕管线前由原生 `<video>` 直接播放，浏览器媒体栈能够处理部分 HEVC、E-AC-3 等组合；页面 JavaScript
却不能读取没有 CORS 的远程 Matroska 字节，也不能要求浏览器把 Matroska 内嵌字幕暴露成 `textTracks`。把远程字幕选择绑定到
MSE/remux 会破坏原生音视频兼容性，并让媒体字节经过 Lux Range Relay。

## 决定

1. 远程 HTTP(S) STRM 的音频、视频、暂停、seek、进度和播放会话仍由 Lux 页面中的原生 `<video>` 负责。页面媒体源恢复为既有
   `proxyUrl`/307 直连路径；307 只用于鉴权和解析，媒体字节从重定向后的远端地址进入浏览器，不由 Lux 转发。
2. 用户选择远程 Matroska/WebM 的 SRT、ASS 或 SSA 后，页面通过 `window.postMessage` 将当前直连 URL 和轨道选择发送给可选 MV3
   扩展。扩展 Service Worker 只按串行 Range 读取 Tracks、SeekHead、Cues 和当前 Cluster，解析安全字幕 cue，再通过 content script 返回
   页面覆盖层；不生成外挂文件、不落盘、不创建第二条视频或字幕媒体连接。
3. （已由 ADR-041 部分取代）扩展最初使用 `declarativeNetRequestWithHostAccess` 为当前 Lux 页面发起的远程媒体请求补齐 CORS
   响应头；现行实现不再修改原生媒体响应，而是由 Service Worker 自己用主机权限读取字幕 Range。扩展不允许 SMB、FTP、路径型 STRM、DRM
   或无有效 `206 + Content-Range` 的媒体。
4. 扩展不可用、权限被拒绝、Range/索引/字幕解析失败时，只显示“远程字幕不可用”，保持同一个原生媒体 URL、播放状态、进度和播放会话；不得
   回退到 Lux 字幕 URL、Lux 媒体 Range Relay 或销毁音视频引擎。
5. 首版不让扩展独立接管全媒体解码。Chrome 扩展可以跨域读取字节，但把任意 HEVC/E-AC-3 解码帧安全注入页面需要独立的 WebCodecs/WebAudio
   播放器、同步和 seek 管线，另行设计和验证。

## 后果

- 原生音视频的兼容性不再因选择字幕而改变，尤其避免 HEVC + E-AC-3 被错误送进不支持的 MSE 组合。
- 安装扩展后，远程 SRT/ASS/SSA 可以在没有 CDN CORS 的情况下由浏览器端读取和渲染；媒体内容不经过 Lux 服务端。
- 扩展需要较宽的远程主机权限，安装包和页面必须明确提示这一点；没有安装扩展时，原生视频仍可播放，只有远程内嵌字幕不可用。

## 与 Jellyfin Web 的对照

Jellyfin Web 并不依赖浏览器把 Matroska 内嵌字幕暴露为 `HTMLMediaElement.textTracks`。其播放器在
`src/plugins/htmlVideoPlayer/plugin.js` 中根据 `DeliveryMethod` 判断是否使用原生嵌入轨；对于外部或不支持原生的
字幕，调用 `playbackManager.getSubtitleUrl(...)`，再把服务端返回的字幕 URL 加载为文本轨。服务端的
`SubtitleController.GetSubtitle` 会通过 `SubtitleEncoder` 输出指定格式，`VideosController.GetVideoStream` 则接收
`subtitleStreamIndex`/`subtitleMethod` 并在需要时交给 FFmpeg 处理。远程 `.strm` 的服务端探测路径也会把
`ShortcutPath` 交给媒体探测器读取。

这能在不安装浏览器扩展的情况下工作，但字幕读取/抽取发生在 Jellyfin 服务端；它不是“远程文件已经到达浏览器，
浏览器自动暴露全部 Matroska 轨道”。Lux 的 `.strm` 边界禁止服务端探测、抽取或代理媒体字节，因此不能直接采用
Jellyfin 的方案而同时保持本 ADR 的流量边界。扩展方案是为该边界增加的可选浏览器侧读取器，而不是复制 Jellyfin
的服务端字幕接口。

## 验证

- Web 单测确认远程原生 `<video>` 使用 `proxyUrl`，字幕选择不调用客户端 MKV 引擎、不重建播放会话，桥接 URL 为当前直连 URL。
- MV3 构建检查确认 manifest、content script、Service Worker 和 Range 边界均可加载；Release 附件为可直接“加载已解压的扩展程序”的 ZIP。
- 真实 Chrome 发布前需在目标站点复查：远程音视频请求在 307 后直达 CDN，扩展 Range 请求返回 206，页面无 `/range`、`/subtitles/...` 或并行
  Lux 媒体字节请求；Chrome 原生支持的音频继续可听，选择字幕后 cue 出现在覆盖层。
