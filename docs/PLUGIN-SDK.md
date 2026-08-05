# Lux Plugin SDK v1

## 目标

Lux 插件是放在 `/config/plugins` 中、由服务重启后发现的独立插件包。首版使用 `.zip` 包格式和独立进程运行时。插件不直接访问 Lux 数据库、媒体根目录或内部任务对象。除元数据插件外，SDK v1 也支持由 Lux 宿主调度的媒体探测插件；任务、媒体库选择、结果持久化和旁车写入仍由宿主负责。

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

`type` 当前允许 `metadata` 和 `media_probe`。媒体探测插件必须同时声明
`category: "MEDIA"` 和 `capabilities: ["media.probe"]`。例如内置的
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
    {"key": "writeSidecars", "label": "写入 mediainfo.json", "type": "toggle", "defaultValue": true}
  ],
  "permissions": {
    "network": ["media-source"],
    "filesystem": []
  }
}
```

Lux 不再要求插件包使用 Lux Ed25519 签名。运行时仅兼容读取历史签名字段，签名不会作为插件发现
或启动的阻断条件；当前打包器始终只生成普通包。插件仍必须通过 ZIP 大小、文件数量、路径、
manifest、格式版本、协议版本、平台入口和声明文件 SHA-256 校验；插件继续在独立进程中运行。

构建 TMDb 包：

```bash
./scripts/package-tmdb-plugin.sh
```

脚本输出 `org.lux.tmdb-<version>.zip`，只包含 `manifest.json` 和当前平台的
`binaries/<platform>-<arch>/lux-plugin-tmdb`。插件目录只在 Lux 重启时扫描，升级包后需要重启。

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
- `media.probe`：接收一个已由 Lux 宿主校验的远程媒体地址，返回受限的 format 和 stream 信息。
- `plugin.shutdown`：请求插件优雅退出。

配置字段支持 text、password、select、toggle 和 number；select 可通过 multiple: true 声明多选，选项使用
`{ "value": "...", "label": "..." }`。`number` 可以声明 `minimum`、`maximum` 和
`defaultValue`。select 可以声明 `optionsSource`，当前支持 `media-libraries`，由 Lux 根据当前
媒体库动态填充选项，不把媒体库 ID 或路径写死在插件包中。管理 API 返回的 `configValues` 只允许包含非敏感当前值。

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

TMDb 插件的首选语言由管理员配置覆盖请求中的默认语言。语言回退开关开启后，电影、剧集、
季和集详情会按配置的备选语言顺序逐字段补全；默认首选语言为 zh-CN，默认备选语言为
zh-SG、zh-HK、zh-TW，开关默认关闭。替代 API 地址开关默认关闭；开启后可选择官方地址、
`https://api.tmdb.org` 或填写自定义 HTTP(S) 基础地址。地址由宿主校验，不得包含凭据、查询参数
或片段。

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
