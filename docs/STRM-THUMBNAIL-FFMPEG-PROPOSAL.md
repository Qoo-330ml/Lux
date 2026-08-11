# STRM 缩略图补全方案（ffprobe + ffmpeg 两步式）

状态：已实现。

## 1. 目标

在现有 org.lux.strm-media-info 插件中增加 STRM 缩略图补全能力，同时保持媒体信息提取和缩略图补全两个功能彼此独立：

- mediaInfoEnabled：使用 ffprobe 提取并保存媒体信息 JSON。
- thumbnailEnabled：使用 ffmpeg 从外部媒体地址截图，并保存为 STRM 的 *-thumb.jpg。

两个开关都由插件配置控制，缩略图开关默认关闭。缩略图功能只处理 .strm 媒体源，不改变本地视频缩略图任务。

## 2. 明确不做的事情

- 不引入 ffmpeg-next、FFmpeg C API 或其他原生 FFmpeg 库依赖。
- 不读取、解析或判断 .strm 文件内的字符串格式。
- 不因为地址是私网 IP、公网 IP、域名、路径或其他非 URL 字符串而拒绝。
- 不把网络媒体完整下载到 Lux 内存或本地。
- 不让 HTTP 请求同步执行 ffprobe/ffmpeg；所有操作仍由后台 STRM 任务完成。

STRM 的外部媒体地址沿用扫描阶段已经登记到 media_sources.external_url 的值，插件只把它作为不透明输入传给命令行工具。

## 3. 执行流程

### 3.1 只开启媒体信息

1. 后台任务检查是否已有媒体信息。
2. 缺失或选择覆盖时，调用一次 ffprobe。
3. 解析并保存媒体信息及可选旁车 JSON。
4. 不调用 ffmpeg。

### 3.2 只开启缩略图

1. 后台任务检查同目录 *-thumb.jpg 以及数据库登记的缩略图是否有效。
2. 缺失时调用一次轻量 ffprobe，只获取视频时长。
3. 按 duration × thumbnailPositionPercent% 计算截图时间点，默认 30%。
4. 调用一次 ffmpeg，从该时间点输出一张 JPEG。
5. 原子写入缩略图并登记 item_images。
6. 不保存完整媒体信息。

这里仍然是两个独立工具各司其职；因为 ffmpeg 的 -ss 需要具体时间值，不能直接可靠地把百分比作为输入。

### 3.3 同时开启媒体信息和缩略图

1. ffprobe 一次输出完整媒体信息，同时复用其中的 duration。
2. ffmpeg 根据 duration 的 thumbnailPositionPercent% 截取一张 JPEG。
3. 分别保存媒体信息和缩略图。

因此，同一个 STRM 同时开启两个功能时会有一次 ffprobe 和一次 ffmpeg。如果缩略图已存在且有效，则跳过 ffmpeg；如果媒体信息也已存在且任务不是覆盖模式，则可只执行缩略图相关流程或直接跳过。

## 4. ffmpeg 截图约束

建议命令参数：

~~~text
ffmpeg
  -hide_banner
  -loglevel error
  -nostdin
  -y
  -ss <duration * thumbnailPositionPercent / 100>
  -i <external_media_address>
  -frames:v 1
  -an
  -vf scale='min(1024,iw)':-2
  -f image2
  <temporary-jpeg>
~~~

- 时间点由 `thumbnailPositionPercent` 配置，默认是视频时长的 30%，允许范围为 1-99。
- 输出最长边限制为 1024，避免生成过大的图片。
- 先写随机临时文件，再用原子替换写入正式文件。
- 校验 JPEG 头尾、文件类型和大小上限。
- 截图失败时任务标记失败，不生成一张错误或空白图片。
- 无法得到有效 duration 时不猜测时间点，记录明确失败原因。

## 5. 配置和任务模型

插件 manifest 增加两个独立 toggle 和一个截图位置 number：

~~~json
{
  "key": "mediaInfoEnabled",
  "type": "toggle",
  "defaultValue": true
}
{
  "key": "thumbnailEnabled",
  "type": "toggle",
  "defaultValue": false
}
{
  "key": "thumbnailPositionPercent",
  "type": "number",
  "defaultValue": 30,
  "minimum": 1,
  "maximum": 99
}
~~~

STRM 任务记录两个开关和 `thumbnailPositionPercent` 的快照，确保手动执行、定时执行、重试任务使用同一组配置。旧配置没有这些字段时分别按 true、false 和 30 兼容。

缩略图补全只针对“缺失或无效”的缩略图，不因媒体信息的覆盖选项而自动覆盖已有缩略图；如后续需要覆盖缩略图，应增加单独的覆盖选项。

## 6. 网络、内存和磁盘控制

- ffprobe 和 ffmpeg 都使用现有后台并发上限，避免同时打开过多网络媒体。
- 每个命令设置独立超时，并在超时后终止子进程。
- 不读取整个媒体文件到 Lux 内存，命令行工具自行按协议读取所需数据。
- 两个命令分别打开输入，因此某些远程源可能产生两次 Range/连接行为；这是接受“两步式”后的明确取舍。
- 缩略图默认关闭，开启后每个 STRM 增加一张 JPEG 的磁盘占用。
- 继续使用系统已有的 ffprobe 和 ffmpeg，Docker 只需保留现有运行时 FFmpeg，不增加 Rust 原生链接和镜像内重复静态库。

## 7. 安全边界

- 外部地址按不透明字符串校验：非空、长度受限，不解析 IP、域名或协议类型。
- 本地输出路径必须由已校验的库根目录和 STRM 相对路径生成。
- 禁止缩略图目标路径穿越库根目录、符号链接替换和超大输出。
- 日志不记录完整外部地址，不记录命令行中的敏感参数。

## 8. 测试计划

### 插件协议和配置

- 新增请求字段能正确传递两个开关。
- 旧请求没有字段时保持媒体信息提取兼容。
- 新 manifest 显示两个独立开关。
- 旧配置迁移后默认值正确。

### STRM 任务

- 只开媒体信息：调用 ffprobe，不调用 ffmpeg。
- 只开缩略图：先取得 duration，再调用 ffmpeg，不保存完整媒体信息。
- 两者都开：ffprobe 和 ffmpeg 各执行一次。
- 已有有效缩略图时跳过 ffmpeg。
- 外部地址覆盖私网地址、域名、路径和普通字符串时不被地址类型拦截。
- ffprobe、ffmpeg 超时或输出无效时任务失败且不留下临时文件。
- 生成的 JPEG 正确写入 *-thumb.jpg 并登记到 item_images。

### 验证命令

~~~text
cargo fmt --all -- --check
cargo build --locked
cargo test --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
~~~

## 9. 已确认行为

1. duration 无法获取时，按方案标记失败，不回退到视频开头截图。
2. thumbnailEnabled 只补全缺失缩略图，不覆盖已有缩略图。
3. `thumbnailPositionPercent` 缺失时使用 30，超出 1-99 时拒绝配置。
