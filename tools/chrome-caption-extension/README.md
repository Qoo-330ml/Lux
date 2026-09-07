# Lux Remote Matroska Captions Chrome Extension

这个 MV3 扩展是 Lux 远程 URL 型 STRM 的可选旁路：

- 原生组合继续优先使用 `<video>`；当浏览器不能播放远程 Matroska 组合时，扩展提供单一 Range 会话，Lux 的
  Matroska Worker/MSE 负责输出视频，AC-3/E-AC-3 在 Worker 中使用窄 WASM 解成 PCM 并由 Web Audio 输出。
  媒体会话不创建第二条原生远程连接。
- 扩展后台使用当前 Lux 播放会话的签名 direct URL 发起 Range 请求；Lux 只返回一次 307，后续请求跟随到远端地址，读取 Matroska 的 Tracks、SeekHead、Cues 和字幕 Cluster，仅把 SRT/ASS/SSA cue 传给 Lux 页面。
- 扩展 Service Worker 使用自身的主机权限读取远程 Range；它不修改原生 `<video>` 的响应头，也不拦截或改变页面的音视频请求。字幕读取不携带页面 Cookie，Lux 服务器只参与一次授权/307 解析，媒体内容从 307 之后的远端地址到浏览器。
- 只允许 HTTP(S) URL；不会读取 SMB、FTP、路径型 STRM、DRM 或不带有效 Range 的资源。

当前可选媒体路径覆盖：

- H.264、VP9、AV1：优先使用浏览器 MSE 的 fMP4 输出；
- HEVC：浏览器 MSE 可用时直通，否则使用现有 HEVC WASM→H.264 fallback；
- AAC、Opus：使用 MSE 音频轨；
- AC-3、E-AC-3：扩展媒体会话使用 `@audio/decode-eac3` 解码为 PCM，要求 Web Audio 可用；
- DTS、TrueHD、PGS/SUP、Dolby Vision 以及无有效 Range 的资源仍显示明确不支持。

扩展不会把媒体字节上传到 Lux。页面只把签名播放入口交给扩展，扩展跟随一次 307 后直接向远端发起串行
Range；Range 数据经页面桥接进入现有 Worker/MSE，只保留在当前播放会话内，不落盘、不整文件下载。

这不是对所有浏览器和所有 4K 文件的实时保证：HEVC 软件解码、Web Audio PCM 调度和远端服务器的 Range 实现
仍需在真实设备上验收。

## 构建

在仓库根目录执行：

```bash
pnpm --dir web exec vite build --config ../tools/chrome-caption-extension/vite.config.ts
```

构建产物在 `tools/chrome-caption-extension/dist/`。在 Chrome 打开 `chrome://extensions`，启用“开发者模式”，点击“加载已解压的扩展程序”选择该目录。

扩展需要远程主机权限，因为远程 STRM 的 CDN 通常没有 CORS；权限只用于当前 Lux 页面对应的字幕 Range 请求。发布包由 `scripts/build-chrome-caption-extension.sh` 生成。
