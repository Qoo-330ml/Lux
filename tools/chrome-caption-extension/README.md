# Lux Remote Matroska Captions Chrome Extension

这个 MV3 扩展是 Lux 远程 URL 型 STRM 的可选旁路：

- 原生 `<video>` 继续直接播放音频和视频；扩展不接管媒体元素、不创建 MSE、不把媒体字节上传到 Lux。
- 扩展后台使用当前 Lux 播放会话的签名 direct URL 发起 Range 请求；Lux 只返回一次 307，后续请求跟随到远端地址，读取 Matroska 的 Tracks、SeekHead、Cues 和字幕 Cluster，仅把 SRT/ASS/SSA cue 传给 Lux 页面。
- 扩展 Service Worker 使用自身的主机权限读取远程 Range；它不修改原生 `<video>` 的响应头，也不拦截或改变页面的音视频请求。字幕读取不携带页面 Cookie，Lux 服务器只参与一次授权/307 解析，媒体内容从 307 之后的远端地址到浏览器。
- 只允许 HTTP(S) URL；不会读取 SMB、FTP、路径型 STRM、DRM 或不带有效 Range 的资源。

由于 Chrome 扩展无法安全地把任意 HEVC/E-AC-3 解码帧直接注入页面的原生 `<video>`，本版本不强制替换播放器。浏览器原生支持的音视频保持原路径，不能原生解码的组合继续由 Lux 现有客户端或服务端能力判定，不会静默切换成一个未经验证的全媒体 WASM 播放器。

## 构建

在仓库根目录执行：

```bash
pnpm --dir web exec vite build --config ../tools/chrome-caption-extension/vite.config.ts
```

构建产物在 `tools/chrome-caption-extension/dist/`。在 Chrome 打开 `chrome://extensions`，启用“开发者模式”，点击“加载已解压的扩展程序”选择该目录。

扩展需要远程主机权限，因为远程 STRM 的 CDN 通常没有 CORS；权限只用于当前 Lux 页面对应的字幕 Range 请求。发布包由 `scripts/build-chrome-caption-extension.sh` 生成。
