# Lux Plugin SDK v1

## 目标

Lux 插件是放在 `/config/plugins` 中、由服务重启后发现的独立插件包。首版使用 `.zip` 包格式和独立进程运行时。插件不直接访问 Lux 数据库、媒体根目录或内部任务对象。除元数据插件外，SDK v1 也支持由 Lux 宿主调度的媒体探测、IP 归属地、通知和弹幕插件；任务、媒体库选择、通知队列、结果持久化和旁车写入仍由宿主负责。

弹幕插件固定使用 `type: "danmaku"`、`category: "MEDIA"` 和 `danmaku.match` 能力。Lux
只向插件发送不含路径和上游地址的文件名：

```json
{"fileName":"Show.S01E02.1080p.mkv"}
```

插件通过 `danmaku.match` 返回 `MATCHED` 或 `NO_MATCH`；匹配成功时返回 `episodeId`、可选的
`animeId`/`provider` 和受大小限制的 `xmlBase64`。插件负责上游请求、匹配回退、超时、响应
限制和 XML 校验，宿主负责最终 XML 校验、原子写入同名 `.xml` 旁车、任务进度和 ACL。弹幕
插件配置由 manifest 的 `configFields` 声明，不能通过普通服务器设置保存。

## 包格式

示例：

```text
org.lux.tmdb-1.0.0.zip
├── manifest.json
├── binaries/
│   ├── linux-x86_64/lux-plugin-tmdb
│   ├── linux-aarch64/lux-plugin-tmdb
│   ├── darwin-arm64/lux-plugin-tmdb
│   └── windows-x86_64/lux-plugin-tmdb.exe
├── assets/icon.png
└── signature.json       # 仅历史包兼容
```

开发时可以把同样的内容解压为 `/config/plugins/org.lux.tmdb/`。生产包的入口必须是 manifest 中的相对路径，禁止绝对路径和 `..` 路径。

## 插件商店目录

Lux 默认从 `https://github.com/Qoo-330ml/Lux-plugins` 读取插件目录；GitHub 仓库地址解析为
`main/index.json`。管理员也可以配置其他 HTTPS `index.json` 地址。目录使用以下 v1 格式：

```json
{
  "formatVersion": 1,
  "plugins": [{
    "id": "org.lux.example",
    "name": "Example",
    "description": "Example plugin",
    "category": "UTILITY",
    "version": "1.0.0",
    "runtime": "process",
    "capabilities": ["utility.example"],
    "packages": [{
      "platform": "linux",
      "arch": "x86_64",
      "url": "https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.example-1.0.0-linux-x86_64.zip",
      "sha256": "<64 lowercase hexadecimal characters>"
    }, {
      "platform": "linux",
      "arch": "aarch64",
      "url": "https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.example-1.0.0-linux-aarch64.zip",
      "sha256": "<64 lowercase hexadecimal characters>"
    }]
  }]
}
```

插件仓库只提交源码和 manifest；workflow 分别在 AMD/x86 与 ARM runner 编译，并把带版本和架构的 ZIP
上传到 GitHub Release，再将 Release 地址和 SHA-256 写入 `packages`。Lux 按当前运行平台选择对应包，
只下载目录声明的 ZIP，并在安装前执行包大小、路径、manifest、平台入口和 SHA-256 校验。目录项的插件 ID、
版本、目标平台和包哈希必须唯一且符合格式限制。旧目录也可以继续使用单个 `package` 字段作为兼容格式。

## Manifest

```json
{
  "formatVersion": 1,
  "id": "org.lux.tmdb",
  "name": "TMDb Metadata",
  "description": "TMDb metadata and image provider",
  "version": "1.0.0",
  "apiVersion": "1",
  "runtime": {
    "kind": "process",
    "entrypoint": "binaries/${platform}-${arch}/lux-plugin-tmdb"
  },
  "type": "metadata",
  "category": "SCRAPER",
  "supportedItemTypes": ["Movie", "Series", "Season", "Episode", "Person", "BoxSet"],
  "capabilities": [
    "metadata.search",
    "metadata.get",
    "metadata.images",
    "metadata.credits",
    "metadata.externalIds",
    "metadata.trailers"
  ],
  "configFields": [{
    "key": "preferredLanguage",
    "label": "首选语言",
    "type": "select",
    "required": true,
    "options": [{"value": "zh-CN", "label": "简体中文"}]
  }, {
    "key": "alternateApiEnabled",
    "label": "替代 API 地址",
    "type": "toggle",
    "required": false
  }, {
    "key": "apiBaseUrl",
    "label": "TMDb API 地址",
    "type": "select",
    "required": true,
    "options": [
      {"value": "official", "label": "https://api.themoviedb.org"},
      {"value": "alternate", "label": "https://api.tmdb.org"},
      {"value": "custom", "label": "自定义"}
    ]
  }],
  "permissions": {
    "network": [],
    "filesystem": ["plugin-cache"]
  },
  "files": []
}
```

`formatVersion` 是包格式；`apiVersion` 是 RPC 契约；`version` 是插件自身版本。三者不能混用。

`type` 当前允许 `metadata`、`media_probe`、`ip_location`、`strm_resolver`、`chapter_detector` 和
`data_migration`。媒体探测插件必须同时声明
`category: "MEDIA"` 和 `capabilities: ["media.probe"]`。例如商店中的
`org.lux.strm-media-info` 使用以下 manifest 核心字段：

```json
{
  "id": "org.lux.strm-media-info",
  "type": "media_probe",
  "category": "MEDIA",
  "capabilities": ["media.probe"],
  "configFields": [
    {
      "key": "libraryIds",
      "label": "媒体库",
      "type": "select",
      "multiple": true,
      "required": true,
      "optionsSource": "media-libraries"
    },
    {
      "key": "concurrency",
      "label": "并发数",
      "type": "number",
      "defaultValue": 2,
      "minimum": 1,
      "maximum": 64
    },
    {
      "key": "existingInfoPolicy",
      "label": "已有媒体信息处理方式",
      "type": "select",
      "defaultValue": "SKIP",
      "options": [
        {"value": "SKIP", "label": "跳过已有媒体信息"},
        {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}
      ]
    },
    {"key": "mediaInfoEnabled", "label": "提取媒体信息", "type": "toggle", "defaultValue": true},
    {"key": "thumbnailEnabled", "label": "补全 STRM 缩略图", "type": "toggle", "defaultValue": false},
    {"key": "thumbnailPositionPercent", "label": "缩略图位置", "type": "number", "required": true, "defaultValue": 30, "minimum": 1, "maximum": 99},
    {"key": "writeSidecars", "label": "写入 mediainfo.json", "type": "toggle", "defaultValue": true}
  ],
  "permissions": {
    "network": ["media-source"],
    "filesystem": []
  }
}
```

Lux 不再要求插件包使用 Lux Ed25519 签名。运行时仅兼容读取历史签名字段，签名不会作为插件发现
或启动的阻断条件；外部插件仓库的构建流程生成普通包。插件仍必须通过 ZIP 大小、文件数量、路径、
manifest、格式版本、协议版本、平台入口和声明文件 SHA-256 校验；插件继续在独立进程中运行。

Lux 主仓库不再包含插件实现或打包脚本。插件源码、manifest 模板和构建脚本位于
[Lux-plugins](https://github.com/Qoo-330ml/Lux-plugins)，向其 `main` 分支提交后由 GitHub
Actions 在 `ubuntu-24.04` 与 `ubuntu-24.04-arm` runner 上分别构建。Release 资产命名为
`<plugin-id>-<version>-linux-x86_64.zip` 或 `<plugin-id>-<version>-linux-aarch64.zip`；商店
`index.json` 按平台列出 Release URL 和 SHA-256。Lux 选择当前平台的资产，校验后自动保存为
`/config/plugins/<plugin-id>-<version>.zip`，插件目录只在 Lux 重启时扫描，升级包后需要重启。

## RPC 方法

消息使用 JSON-RPC 风格的 `id`、`method`、`params`、`result` 和 `error` 字段，通过插件进程 stdin/stdout 传输。插件 stdout 只允许输出协议消息，诊断日志写 stderr。

方法：

- `plugin.hello`：协商协议、返回 manifest 摘要和运行状态。
- `plugin.health`：返回插件是否可用，不返回凭据。
- `metadata.search`：返回 Emby 风格 `RemoteSearchResult` 列表。
- `metadata.get`：返回 `MetadataResult<T>`。
- `metadata.images`：返回 `RemoteImageInfo` 列表。
- `metadata.credits`：返回演员/人物的 provider-neutral cast 列表。
- `metadata.externalIds`：返回 `ProviderIds`。
- `metadata.trailers`：返回预告片候选。
- `media.probe`：接收一个已由 Lux 宿主校验的媒体地址，以及 `includeMediaInfo`、
  `includeThumbnail` 和 `thumbnailPositionPercent`。媒体信息由 `ffprobe` 返回受限的 format/stream 信息；
  缩略图由 `ffmpeg` 在 duration 的 `thumbnailPositionPercent` 百分比位置生成 JPEG（缺省为 30），并通过受限的
  `thumbnailJpegBase64` 返回。宿主可以把成功截图以同一文件同时登记为 `POSTER` 和 `THUMB`。插件不解析 `.strm` 内容，也不因地址类型拒绝输入。
- `ip.location`：接收一个已由 Lux 宿主校验的公网 IP，返回统一的归属地字段；第三方供应商协议只存在于插件进程。
- `chapters.detect`：接收同一季度至少两个分集的有界 Chromaprint 指纹序列。每个分集只包含请求内临时 `key`、固定 `sampleRate: 11025`、`fingerprintPointDurationTicks: 1238095`、指纹 Base64、窗口起点和窗口时长；宿主对每个文件固定取第一个音频流并让 FFmpeg chromaprint muxer 输出 raw `uint32` 点序列，按 little-endian 编码，不能把 Base64 字节索引当作时间。插件不得接收路径、URL、媒体源 ID 或任务对象。结果只能返回 `IntroStart`、`IntroEnd` 和 `CreditsStart`，时间必须落在对应窗口内，置信度为 0-1。
- `chapters.lookup`：接收请求内临时 `key`、TMDb/TVDb/IMDb ID、季号、集号和可选时长；插件不得接收路径、URL、媒体源 ID、音频指纹或任务对象。插件只能访问 manifest 声明的固定网络主机，返回 `IntroStart`、`IntroEnd` 和 `CreditsStart`；无数据时返回空标记，宿主不会因空响应删除已有标记。
- `migration.test`：接收管理员提交的 Emby 基础地址、API key 和明确的局域网访问许可，返回脱敏服务器信息及
  `historyCapability`。基础地址不得含凭据、查询参数或片段；API key 不得出现在结果、日志或错误消息中。
- `migration.list_users`：分页边界由宿主提供，插件调用 Emby 用户接口并返回用户名、显示名、启用状态、管理员标记、
  媒体库文件夹策略和头像标签；不返回 Emby token、密码或完整原始响应。
- `migration.list_items`：接收一个 Emby 用户 ID 和有界分页参数，返回 Movie、Series、Season、Episode 的稳定 ID、
  ProviderIds、层级字段和可选的用户状态；未知条目类型不得被伪造成支持类型。
- `migration.user_state`：接收一个 Emby 用户 ID 和有界分页参数，返回已看、播放位置、播放次数、最近播放时间和收藏。
  当前 `historyCapability: "ITEM_STATE"` 只表示条目聚合状态，不得生成假的历史事件。
- `migration.authenticate_user`：仅在 Lux 用户首次登录迁移账户时接收一次用户名和密码，向 Emby 验证后只返回成功及
  脱敏用户身份；插件不得返回或持久化 Emby access token、密码或完整认证响应。
- 迁移插件必须声明 `type: "data_migration"`、`category: "MIGRATION"` 和 `capabilities: ["migration.emby"]`。
  宿主负责迁移任务、映射、导入、幂等、恢复和历史事件落库；插件不能访问 Lux 数据库、媒体目录或任务对象。
- `plugin.shutdown`：请求插件优雅退出。

配置字段支持 text、password、select、toggle 和 number；select 可通过 multiple: true 声明多选，选项使用
`{ "value": "...", "label": "..." }`。`number` 可以声明 `minimum`、`maximum` 和
`defaultValue`。select 可以声明 `optionsSource`，当前支持 `media-libraries`，由 Lux 根据当前
媒体库动态填充选项，不把媒体库 ID 或路径写死在插件包中。管理 API 返回的 `configValues` 只允许包含非敏感当前值。
媒体库动态填充选项，不把媒体库 ID 或路径写死在插件包中。片头片尾插件不得用 `libraryIds` 配置媒体库归属；
媒体库通过 Lux API 的 `chapterSourceId` 选择数据源。管理 API 返回的 `configValues` 只允许包含非敏感当前值。

插件配置通过 `PUT /api/v1/admin/plugins/{pluginId}/config` 保存。媒体探测插件通过
`POST /api/v1/admin/plugins/org.lux.strm-media-info/run` 按已保存配置创建后台任务；旧的
`POST /api/v1/admin/strm-probe-jobs` 也只读取该插件配置。插件进程仍只收到单个 `media.probe`
请求，配置 schema 和任务执行不意味着插件可以访问 Lux 数据库或媒体根目录。

请求和返回数据使用以下稳定名称：

```text
BaseItem
Movie
Series
Season
Episode
Person
BoxSet
MetadataResult
RemoteSearchResult
RemoteImageInfo
ProviderIds
ImageType
```

### 刮削器调用约定

媒体库保存的 `scraperId` 是插件的稳定 ID。Lux 不会根据插件名称或上游服务写死调用路径，所有
媒体库级刮削都通过以下 provider-neutral 请求字段发送：

```json
{
  "itemType": "Movie",
  "name": "片名",
  "year": 2019,
  "language": "zh-CN",
  "providerId": "provider-specific-id"
}
```

搜索返回 `items`，详情返回 `metadata`，图片返回 `images`，演员返回 `cast`。`ProviderIds` 的
值必须是字符串，键由插件定义（例如 `Tmdb`、`Douban`）；Lux 只把它当作不透明 ID 保存，不能
假设是数字或 TMDb URL。图片必须返回完整 HTTPS `Url`，并声明 `Type`、语言和可选尺寸。
插件缺少某种媒体类型或能力时，应返回 `PLUGIN_PROVIDER_NOT_FOUND`，不能伪造空的 TMDb 数据。

图片请求的 `language` 为空字符串时表示手动搜索的“不限语言”模式；插件此时不得套用管理员的
元数据首选/备选语言，也不得添加 `language` 或 `include_image_language` 过滤条件，应返回上游可用的
所有语言图片。非空 `language` 仍按普通请求处理，批量刮削的语言策略不变。

TMDb 插件的首选语言由管理员配置覆盖请求中的默认语言。语言回退开关开启后，电影、剧集、
季和集详情会按配置的备选语言顺序逐字段补全；默认首选语言为 zh-CN，默认备选语言为
zh-SG、zh-HK、zh-TW，开关默认关闭。替代 API 地址开关默认关闭；开启后可选择官方地址、
`https://api.tmdb.org` 或填写自定义 HTTP(S) 基础地址。地址由宿主校验，不得包含凭据、查询参数
或片段。

### IP 归属地调用约定

IP 归属地插件必须声明 `type: "ip_location"`、`category: "NETWORK"` 和
`capabilities: ["ip.location"]`。请求格式为 `{"ip":"8.8.8.8"}`，返回格式为：

`{"ip":"8.8.8.8","country":"美国","province":null,"city":null,"district":null,"street":null,"isp":"示例运营商","latitude":null,"longitude":null}`

`ip` 必须与查询地址相同；Lux 会再次校验 IP、字段长度和响应大小，并把无效结果视为插件失败。
默认使用已安装的 `org.lux.qoo-ip138`（显示名称“ip138 IP归属地查询”）。如果安装了其他
`ip_location` 插件，Lux 会停用 ip138，并只使用其他已安装的归属地插件；Hiofd 显示名称为“IP归属地查询增强”。
成功结果只放在进程内 24 小时缓存，失败结果只放 5 分钟；不写入 SQLite、不提供公开查询接口。
插件负责第三方 HTTP/HTML/JSON 解析，不得返回凭据、完整第三方响应、签名字段或完整上游 URL。

### `.strm` 目标解析调用约定

`.strm` 协议解析插件必须声明 `type: "strm_resolver"`、`category: "MEDIA"` 和
`capabilities: ["strm.resolve"]`。Lux 只把 SMB/FTP 原始目标发送给该能力，
请求格式为：

```json
{"target":"smb://nas/media/movie.mkv"}
```

插件成功时返回 `{"status":"RESOLVED","url":"https://media.example.invalid/movie.mkv"}`；
不支持该目标时返回 `{"status":"UNSUPPORTED"}`。Lux 按插件 ID 稳定顺序尝试已安装、启用且
配置有效的解析器，插件可以自行判断目标是否适用。宿主会再次校验返回地址必须是无凭据、无
fragment、无控制字符且长度受限的 HTTP(S) URL；不合格结果不会下发给客户端。播放请求只在
解析成功后临时重定向到结果地址，Lux 不代理媒体字节，插件也不能访问 Lux 数据库或媒体根目录。

### 媒体探测调用约定

媒体探测插件只处理单个请求，不拥有媒体库扫描权限。请求格式为：

```json
{
  "url": "https://media.example.invalid/video.mkv"
}
```

返回结果只包含 Lux 允许写入 `media_sources` 和 `media_streams` 的字段：

```json
{
  "container": "matroska",
  "sourceSize": 1234,
  "durationTicks": 125000000,
  "bitrate": 500000,
  "streams": [
    {
      "streamIndex": 0,
      "streamType": "VIDEO",
      "codec": "h264",
      "language": null,
      "title": null,
      "isDefault": false,
      "isForced": false,
      "details": {}
    }
  ]
}
```

Lux 在发送请求前执行协议、主机和地址策略校验，并在收到结果后再次校验字段数量、大小、索引、枚举和数值范围。插件不得返回完整 URL、认证信息或原始 `ffprobe` JSON。插件错误使用稳定代码，例如
`MEDIA_PROBE_INVALID_URL`、`MEDIA_PROBE_TIMEOUT`、`MEDIA_PROBE_PROCESS_FAILED` 和
`MEDIA_PROBE_INVALID_OUTPUT`；错误消息不能包含完整 URL 或 stderr。

媒体探测调用只能由后台 STRM 探测任务触发，不得从播放、PlaybackInfo 或普通用户请求路径触发。Lux 宿主负责并发、超时、取消、重启恢复、任务状态、数据库写入和可选的
`*-mediainfo.json` 原子写入。`permissions.network` 是能力声明，不替代宿主的出站 URL 安全策略。

### 通知器调用约定

通知器必须声明 `type: "notification"`、`category: "NOTIFICATION"` 和
`capabilities: ["notification.send"]`。宿主通过 `notification.send` 发送：

```json
{
  "event": {
    "schemaVersion": 1,
    "eventId": "event-1",
    "eventType": "MEDIA_ADDED",
    "occurredAt": 1700000000,
    "serverId": "server-1",
    "data": {"libraryId": "library-1", "addedCount": 1}
  },
  "target": {
    "url": "https://example.com/lux-hook",
    "allowPrivateNetwork": false
  },
  "config": {"payloadFormat": "LUX"},
  "secret": "provider-specific-secret"
}
```

`event.data` 只包含 Lux 事件白名单字段，不包含本地绝对路径、`.strm` 原始目标、令牌或不必要的用户
隐私字段。`target` 是通知器投递所需的目标信息，`url` 为空时由通知器根据自身配置决定目标；
`allowPrivateNetwork` 是 Lux 已批准的私网策略，通知器必须继续执行危险保留地址检查且不得自行放宽策略。
`config` 只能保存非秘密目标设置；密码、Token、API Key 等必须放入受控的 `secret` 字段。
插件必须返回 `DELIVERED`、`RETRYABLE` 或 `FAILED`，可选返回 provider 请求 ID、重试秒数和稳定错误码：

```json
{"status":"DELIVERED","providerRequestId":"message-1"}
```

宿主负责超时、重试、退避、投递记录和失败恢复。通知插件进程不会获得 `LUX_CONFIG_DIR`，不能通过文件系统
读取其他插件或服务器 Secret。Webhook、Telegram、企业微信等平台的 payload 和认证逻辑属于各自插件，不应
写入通知核心。

## 错误码

```text
PLUGIN_INVALID_REQUEST
PLUGIN_TIMEOUT
PLUGIN_UNAVAILABLE
PLUGIN_RATE_LIMITED
PLUGIN_AUTH_FAILED
PLUGIN_PROVIDER_NOT_FOUND
PLUGIN_INVALID_RESPONSE
PLUGIN_INTERNAL_ERROR
```

插件返回的元数据是不可信输入。Lux 必须验证字段大小、枚举、URL、Provider ID 和图片类型，之后才允许写入数据库、NFO 或 Emby API 响应。

## TMDb 插件

`org.lux.tmdb` 是对 Emby `MovieDb.dll` 行为的独立重写，不加载原始 DLL。功能范围包括电影、剧集、季、集、人物、合集、图片、外部 ID、预告片、语言、缓存、限流、重试和 TMDb v3 API Key。插件使用自己的缓存目录；Lux 负责最终元数据持久化、NFO/图片回写和 Emby DTO 映射。
