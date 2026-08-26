# LUX-198：Web 播放会话、服务端 HLS 与 Jellyfin FFmpeg 7

## 当前切片

本任务拆成可独立验证的切片；每个切片完成后保持仓库可编译，并单独提交。

1. 规格和运行时：新增 ADR-026，更新开发规格，安装固定的 Jellyfin `v7.1.4-3` Trixie 包。
2. 播放合同：定义 0～4 档播放计划、`.strm` 只能 Direct 的纯决策函数和 API/TypeScript DTO。
3. Web 会话直放：创建 Web 播放会话、签名 Direct URL、幂等事件和资源生命周期；保留旧进度接口。
4. 服务端 HLS：实现本地媒体的 fMP4/CMAF manifest、segment、进程组和有界回收。
5. Web 引擎：接入 Direct、Safari HLS、MSE/HLS.js 和现有客户端 fallback。
6. 阶段验证：容器双架构、数据库空库迁移、浏览器播放/seek/停止和 `.strm` 无 ffmpeg 验证。

## 当前切片 1：规格和运行时

预计文件：

- `docs/decisions/004-direct-play-only.md`
- `docs/decisions/026-web-playback-and-server-hls.md`
- `docs/LUX-DEVELOPMENT.md`
- `docs/LUX-198-PLAN.md`
- `runtime/Dockerfile`
- `Dockerfile`

本切片不改变 Rust/Web 行为。运行时使用 GitHub Release 固定资产：

- `jellyfin-ffmpeg7_7.1.4-3-trixie_arm64.deb`
- `jellyfin-ffmpeg7_7.1.4-3-trixie_amd64.deb`

构建必须按 `TARGETARCH` 选择资产并校验 SHA-256；不使用浮动的 `latest` URL，不安装普通 Debian `ffmpeg`。

## 不在当前切片提前实现

- 不在 runtime 切片中修改播放 API、数据库或 Web 播放器。
- 不把 `.strm` 交给 ffmpeg，也不增加 `.strm` 代理。
- 不实现字幕转换、DRM、多码率自适应 HLS 或 Emby 转码接口。

## 阶段门

切片 1 通过容器构建、`ffmpeg -version`、`ffprobe -version`、软件编码器存在、ARM64/AMD64 smoke test 后，
再进入播放合同切片。
