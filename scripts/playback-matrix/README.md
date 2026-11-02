# Web 播放兼容性矩阵

把“遇到一个格式才发现一个问题”变成“一次性系统扫描”。

这套工具做两件事：

1. `generate-samples.mjs` 用本机 `ffmpeg` 生成一批 5–10 秒、覆盖典型视频/音频/字幕组合的小样本，按 Lux 目录规范
   `Movies/<名称 (年份)>/<名称 (年份)>.<ext>` 落盘，并输出 `manifest.json`；
2. `run-matrix.mjs` 通过 Playwright 驱动真实浏览器登录测试实例，逐个播放媒体库里的每个 source，
   记录服务端计划、客户端回退、画面/音频是否真的解码、首帧耗时，最后生成 `report.md` + `report.json`。

样本和报告都是本地产物，不进入 git；本工具不修改产品代码、不向 `web/package.json` 添加依赖、也不接入 CI。

---

## 目录

```
scripts/playback-matrix/
├── generate-samples.mjs   # 生成样本 + manifest.json
├── run-matrix.mjs         # 浏览器实测 + report.md/report.json
├── README.md              # 本文件
├── samples/               # 生成物（gitignored）
└── reports/               # 报告与截图（gitignored）
```

---

## 前置条件

- Node.js ≥ 20（脚本只用标准库）。
- `ffmpeg` / `ffprobe` 在 `PATH`，或通过 `FFMPEG` / `FFPROBE` 指向具体可执行文件。
  缺少某个编码器时生成器会自动跳过并记录原因，不会中断整个批次。
- Playwright：沿用 `scripts/browser-smoke.mjs` 的 `createRequire` 方式加载仓库外已有的 Playwright，
  **不要**把它加进 `web/package.json`。如果本机还没有：

  ```bash
  mkdir -p /tmp/lux-e2e && cd /tmp/lux-e2e
  npm init -y && npm install playwright
  cd -
  ```

  然后在运行矩阵时加上：

  ```bash
  NODE_PATH=/tmp/lux-e2e/node_modules node scripts/playback-matrix/run-matrix.mjs
  ```

  如果 Playwright 安装在别处，可用 `PLAYWRIGHT_MODULE_DIRS=<含 playwright 的 node_modules 目录>`。
  找不到时脚本会打印同样的安装提示并以退出码 2 结束。

- Chrome：默认使用本机 `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`，
  也可用 `CHROME_PATH` 指定。`BROWSER=webkit|firefox` 只在 Playwright 已安装对应浏览器时运行，
  否则该浏览器会在报告里记为跳过。
- 无头运行即可覆盖大多数格式兼容问题；HEVC/HDR 等依赖硬件解码的结论请用
  `MATRIX_HEADED=1` 有头运行复验（无头 Chrome 的解码路径与真机窗口不同）。

---

## 1. 生成样本

```bash
# 默认输出到 scripts/playback-matrix/samples（已 gitignore）
node scripts/playback-matrix/generate-samples.mjs

# 输出到临时目录，避免占用仓库目录
node scripts/playback-matrix/generate-samples.mjs --out /tmp/lux-playback-matrix/samples

# 只看计划和跳过项，不编码
node scripts/playback-matrix/generate-samples.mjs --list

# 只重建音频相关样本，强制重编码
node scripts/playback-matrix/generate-samples.mjs --only audio --force
```

常用选项：

| 选项 | 说明 |
| --- | --- |
| `--out <dir>` | 输出根目录，默认 `scripts/playback-matrix/samples` |
| `--duration <5-10>` | 默认样本秒数，默认 5（4K 等大样本单独用 4 秒） |
| `--only <substr>` | 只生成标题/分组包含该子串的样本，可重复 |
| `--force` | 目标文件已存在也重新编码 |
| `--list` | 只打印计划与“无法生成”清单 |
| `--json` | stdout 输出机器可读摘要 |

生成结束后会写入 `<out>/manifest.json`，包含每个样本的特征（编码器、profile、像素格式、尺寸、帧率、VFR/隔行/
旋转/HDR、音轨与声道、字幕）、`ffprobe` 摘要、字节数，以及 **skipped 列表**（编码器缺失或组合无法生成的原因）。
样本总量约 54 个、30 MB 左右，远低于 300 MB。

### 覆盖范围

- 视频：H.264 baseline/main/high/High 10/High 4:2:2/High 4:4:4、HEVC Main/Main10/HDR10、VP8、VP9 8/10-bit、
  AV1 8/10-bit、MPEG-2、MPEG-4 Part 2、MJPEG、ProRes、隔行、60fps、VFR、非标准尺寸 1918×1078、旋转显示矩阵、4K。
- 音频：AAC（LC、mono/stereo/5.1）、MP3、MP2、AC-3（stereo/5.1）、E-AC-3 5.1、DTS core 5.1、TrueHD 5.1、
  FLAC 5.1/7.1、Opus 5.1、Vorbis、PCM s16le/s24le、ALAC 5.1、WMA v2、双音轨、无音轨。
- 字幕：内嵌 SRT、ASS、MP4 `mov_text`、双字幕轨。
- 已声明但本机无法生成：VC-1、PGS、HE-AAC、DTS-HD MA、AC-3/E-AC-3 7.1、Vorbis 5.1。
  它们会写进 `manifest.json` 的 `skipped`，报告也会注明未覆盖。

### 同步到测试库

脚本 **不负责 ssh / 同步**。把输出目录（其中包含 `Movies/`）作为媒体库根目录，或同步进 NAS 测试实例
已有的媒体库根目录，然后：

1. 在管理端触发一次扫描（或等待实时监听）；
2. 等条目探测完成（`mediaSources[].streams` 有视频/音频流）；
3. 用测试账号确认能看到这些条目（普通账号需要媒体库 `canView` 权限）。

只生成了 `.mkv` / `.mp4`，扫描器认得；不需要 `.strm`，本矩阵也不覆盖 `.strm` 源。

---

## 2. 运行矩阵

```bash
BASE_URL=http://192.168.1.10:8097 \
LUX_USER=viewer \
LUX_PASS='...' \
BROWSER=chrome \
node scripts/playback-matrix/run-matrix.mjs
```

密码 **只接受环境变量**，不会写进报告、日志或命令行参数。

### 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `BASE_URL`（或 `LUX_BASE_URL`） | `http://127.0.0.1:8097` | 测试实例地址 |
| `LUX_USER` | 必填 | 测试账号 |
| `LUX_PASS` | 必填（仅环境变量） | 密码 |
| `BROWSER` | `chrome` | `chrome` / `webkit` / `firefox`，可逗号分隔多个 |
| `CHROME_PATH` | `/Applications/Google Chrome.app/...` | Chrome 可执行文件 |
| `MATRIX_HEADED` | 未设置（无头） | `1` 表示有头运行 |
| `MATRIX_TIMEOUT_MS` | `30000` | 单样本最长等待（首帧/进度） |
| `MATRIX_RELEASE_MS` | `1500` | 每个样本后回到 `about:blank` 的会话释放等待 |
| `MATRIX_OUT` | `scripts/playback-matrix/reports` | 报告目录 |
| `MATRIX_LIBRARY` | 空 | 只测名称包含该子串的媒体库 |
| `MATRIX_FILTER` | 空 | 只测标题匹配该正则的条目 |
| `MATRIX_MAX_ITEMS` | 0（全部） | 最多测 N 个 source，便于调试 |
| `MATRIX_SCREENSHOTS` | `1` | `0` 关闭失败/回退截图 |
| `MATRIX_RESET_PROGRESS` | `1` | `0` 关闭播放前重置进度 |
| `PLAYWRIGHT_MODULE_DIRS` | 空 | 额外 node_modules 搜索目录 |

### 运行行为

1. 打开 `BASE_URL`，用登录页输入 `LUX_USER` / `LUX_PASS`（凭据不落盘、不打印）。
2. `GET /api/v1/libraries` 枚举媒体库（可过滤），再 `GET /api/v1/libraries/{id}/items` 以 `pageSize=100`
   分页（服务端上限）取条目；列表缺少 `mediaSources` 时再读 `GET /api/v1/items/{id}`。
3. 对每个 source：先清掉服务端保存的播放进度，再打开 `/watch/<itemId>?sourceId=<sourceId>`；
   等待最多 `MATRIX_TIMEOUT_MS`，期间按需触发一次 `<video>.play()`。
4. 记录：
   - 服务端最终计划：捕获 `/api/v1/playback/bootstrap` 与 `/api/v1/playback/sessions` 响应里的
     `plan.type` / `tier`（DIRECT、REMUX=tier 1、SERVER_HLS tier 2–4、UNSUPPORTED+原因）；
   - 客户端回退：控制台里的 `[Lux 播放器]` 警告及原因（`stalled` / `slow` / `unviable` / `silent-video` /
     `silent-audio` / 直连失败）；
   - `video.videoWidth/Height > 0`、`currentTime` 是否前进 ≥ 1.5 秒；
   - 音频是否真的解码：Chromium 用 `webkitAudioDecodedByteCount > 0`，其他引擎记 `unknown`；
   - `role=alert` 错误横幅、媒体错误、首帧耗时、导航耗时。
5. 每个样本后导航到 `about:blank` 并等待会话释放，**严格串行**，不并发。
6. 捕获到 HTTP 429（同时播放数上限）时按 5s / 10s / 20s 退避重试（最多 3 次），仍失败则样本记 ⏭️ 并写明原因。

---

## 3. 报告与判定

`<MATRIX_OUT>/report.md` 和 `report.json`（默认 `scripts/playback-matrix/reports/`）。`report.md` 包含：

- 运行环境与总数；
- 每个浏览器一张 **视频编码 × 音频编码** 矩阵：

  | 符号 | 含义 |
  | --- | --- |
  | ✅ | 直连，画面与音频正常 |
  | ⚠️ | 播放成功但发生回退：服务端转码/重封装、客户端回退，或音频静默 |
  | ❌ | 失败：无画面、时间未前进、`role=alert` 阻塞提示、媒体错误或 UNSUPPORTED |
  | ⏭️ | 跳过：浏览器未安装、条目无源、429 重试耗尽等 |

  一个格子有多个样本时显示 `⚠️×2` 这样的计数，并取最严重的状态。
- 所有 ❌ 与 ⚠️ 的详情：浏览器、服务端计划、回退原因、控制台错误、首帧耗时；
- 失败/回退样本的截图（`reports/screenshots/`）；
- **可疑规则汇总建议**，例如“某音频编码在所有 Chromium 样本上 `webkitAudioDecodedByteCount=0`
  → 建议加入服务端强制音频转码名单”“某视频编码从未直连 → 检查 remux/字节对齐”。

`report.json` 保留原始结构（每个样本的计划、回退、指标、控制台、截图路径），便于后续做趋势对比。
报告不包含密码、Cookie、令牌或带签名的媒体 URL（查询串会被脱敏）。

### 退出码

| 退出码 | 含义 |
| --- | --- |
| `0` | 没有 ❌（允许 ⚠️ 与 ⏭️） |
| `1` | 至少一个 ❌ |
| `2` | 用法错误、登录失败、找不到 Playwright、没有可用样本等 |

有 ❌ 时非零，便于本地脚本或手动检查时判定“本轮不干净”。

---

## 4. 已知边界

- 不覆盖 `.strm`（工具只生成/测试 `.mkv`、`.mp4`）。
- 不进入 CI；不要把它接进 `scripts/check-all.sh`。
- 音频解码只在 Chromium 引擎可判定；Firefox/WebKit 的静默问题需要人工听音或另配指标。
- 字幕只验证“轨道存在且播放不崩”，不做像素级渲染校验。
- HDR/4K 的显示效果不作像素比较，只验证能否起播与进度；无头 Chrome 的 HEVC/HDR 结论可能与真机不同。
- PGS、HE-AAC、DTS-HD MA、VC-1 等本机无法生成的编码记在 `manifest.json` 的 `skipped`，需要时放入现成样本补齐。
- 媒体库目录由项目所有者手动同步；脚本不会 ssh，也不会修改 NAS 上的文件。

---

## 5. 常见问题

**找不到 playwright**

按上面的临时目录方式安装，或设置 `NODE_PATH` / `PLAYWRIGHT_MODULE_DIRS`。脚本不会修改 `web/package.json`。

**没有可见媒体库 / 没有媒体源**

确认测试账号有媒体库 `canView`，样本已同步并完成扫描与探测；`--library` / `MATRIX_FILTER` 可能过滤掉了全部条目。

**所有样本都是 ⏭️**

检查 `BROWSER` 对应的 Playwright 浏览器是否安装（`npx playwright install webkit`），以及是否触发 429 上限。

**重跑结果和上次不同（从片尾开始）**

脚本默认在播放前把条目进度重置为 0（`MATRIX_RESET_PROGRESS`）。若账号没有 CSRF 或进度接口返回非 200，
报告会记录 `进度重置：HTTP ...`，此时可手动清除“继续观看”后重跑。
