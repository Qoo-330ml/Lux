# ADR-039：远程 STRM 原生音视频与字幕旁路

## 状态

已接受；部分取代 ADR-038 的“远程字幕切换到 MSE”决定。ADR-038 中关于签名 Range Relay、资源边界、失败不影响播放会话的约束继续有效。

## 背景

远程 HTTP(S) STRM 的原生 `<video>` 已经可以稳定解封装并播放 HEVC、E-AC-3 等浏览器媒体组合，但浏览器通常不会把
Matroska 内嵌 SRT/ASS/SSA 暴露为 `video.textTracks`。把字幕能力绑定到 `ClientMkvEngine` 会引入第二套音视频解码条件：
Chrome 不支持 HEVC + E-AC-3 的 MSE 组合时，字幕选择会被错误地报告为不可用，甚至造成无声或播放器引擎失败。

## 决定

1. 远程 HTTP(S) Matroska/WebM 始终由当前原生 `<video>` 负责音频、视频、进度、暂停、seek 和播放会话生命周期。字幕选择不替换
   `src`，不创建或销毁 MSE/客户端音视频引擎。
2. 用户选择远程内嵌 SRT/ASS/SSA 后，JavaScript 仅通过当前播放会话签名的同源 `rangeUrl` 启动字幕旁路读取器。读取器解析有限元数据、
   SeekHead/Cues 和当前时间窗口的 Cluster，将字幕 cue 交给 React 覆盖层；不生成外挂文件、不落盘、不请求 `/subtitles/...`，也不读取原始
   外部 URL。
3. 旁路读取器以单个逻辑读取器、串行 Range、每次最多 32 MiB、Cues 元数据最多 8 MiB 的边界运行。资源变化、Range/索引/字幕解析失败只
   清除字幕状态并显示脱敏的“远程字幕不可用”，不得停止、重建或替换音视频播放。
4. 远程字幕轨道仍使用稳定的 UI 字符串 ID；匹配优先使用轨道名称、语言、文本 codec 和源内支持字幕序号，不把 ffprobe stream index
   当作 Matroska TrackNumber。
5. 本地 Matroska 的客户端 fallback 和现有外置字幕合同不变。`ClientMkvEngine` 不再作为远程字幕的必要条件，但仍可用于本地媒体场景。

## 后果

- Chrome 的 HEVC + E-AC-3 等原生可播放组合不会因为字幕选择而失去音频。
- 远程字幕会产生受播放会话签名保护的旁路 Range 请求；浏览器原生媒体请求仍保持原来的播放路径。
- Cues 缺失或损坏的媒体不会被顺序扫描整部文件；该字幕只显示不可用，音视频仍可继续播放。

## 验证

- Web 单测覆盖 Cue-selected Cluster、SRT/ASS/SSA 解析、Range 边界和轨道匹配。
- 播放流程确认字幕选择后 `video.currentSrc`、当前时间、播放状态和播放会话不变，且不调用 `shouldUseClientMkv`。
- 真实浏览器检查确认原生视频没有 `error`，E-AC-3 音频不因字幕选择丢失，字幕覆盖层随 `currentTime` 更新。
