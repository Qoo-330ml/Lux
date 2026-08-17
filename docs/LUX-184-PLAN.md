# LUX-184：Web 4K 媒体能力探针

## 范围

为 Lux Web 提供一个独立的浏览器媒体能力探针，验证实际媒体文件在原生 `video`、MediaCapabilities 和
WebCodecs 下的能力。探针不接入正式 `PlayerPage`，不实现 WASM 解码器，不触发服务端转码或 Remux。

探针目标是为后续播放器引擎选型提供证据，特别是 4K HEVC Main、HEVC Main10/HDR10、MP4、MKV 和高帧率
媒体。Dolby Vision、DRM 和服务端转码不属于本任务。

## 本地样本要求

样本不得包含用户数据、真实 `.strm` URL、访问令牌或 Cookie。建议在仓库外准备以下文件，并通过页面输入
Lux 同源 stream URL 或本地测试服务器 URL：

| 样本 | 目的 |
|---|---|
| 4K HEVC Main 8-bit + AAC + MP4 | 基础 HEVC 能力 |
| 4K HEVC Main10 + HDR10 + MP4 | 10-bit/HDR10 能力 |
| 4K HEVC + MKV | 容器和 seek 能力 |
| 4K HEVC 60fps | 高帧率和硬件吞吐 |
| 4K H.264 + AAC | 浏览器播放基准 |
| Dolby Vision 样本（可选） | 验证明确报告不在承诺范围 |

真实样本优先于仅由测试图案重新编码的文件；HDR10 元数据、音频轨和关键帧布局都可能影响结果。大文件不
提交到 Git，也不写入项目 fixture。

## 实现

- [x] `web/public/media-capability-probe.html` 提供 URL、MIME、codec、分辨率、码率和帧率输入。
- [x] `web/public/media-capability-probe.js` 探测 `canPlayType`、MediaCapabilities 和 WebCodecs。
- [x] 页面执行 metadata 加载、短时播放、VideoFrame 计数、丢帧和播放位置测量。
- [x] 探针结果不显示完整媒体 URL。
- [x] 4K HEVC Main、HEVC Main10 HDR10 和 H.264 基准预设已加入。
- [x] 新增无浏览器依赖的探针逻辑单测。

## 验证

自动检查：

```bash
node --test web/tests/media-capability-probe.test.mjs
pnpm --dir web test
pnpm --dir web build
```

真实浏览器检查：

1. 启动 Lux 或 Vite Web 开发服务器。
2. 打开 `/media-capability-probe.html`。
3. 对每个样本选择匹配预设并运行探针。
4. 记录浏览器版本、操作系统/设备、Lux 提交、`uname -m`、样本校验值和 JSON 结果。
5. 将脱敏结果写入 `docs/COMPATIBILITY.md`；不得记录密码、Cookie、令牌、完整外部 URL 或用户数据。

验收前不能把 `canPlayType`、MediaCapabilities 或 WebCodecs 的“supported”字段单独当作 4K 实时播放承诺，
必须有实际播放时长、帧数、丢帧和音画同步观察结果。

## 明确不做

- 不修改正式播放器和现有播放进度事件。
- 不修改 Rust 播放接口、Range 语义、Emby DTO、数据库或 `.strm` 代理行为。
- 不加入 WASM、FFmpeg、WebCodecs 解码 pipeline 或新的核心依赖。
- 不宣称所有浏览器都能播放 4K HEVC。
