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
`main/index.json`。管理员也可以配置最多 10 个其他 HTTPS `index.json` 地址。多个目录合并时按插件
ID 去重，同一 ID 以最高 SemVer 版本为准。目录使用以下 v1 格式：

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

metadata 插件还应声明稳定的 `providerKey`，并可声明用于旧配置兼容的 `aliases`：

```json
{
  "id": "org.lux.tmdb",
  "type": "metadata",
  "category": "SCRAPER",
  "providerKey": "tmdb",
  "aliases": ["tmdb"]
}
```

`pluginId` 只表示安装/运行时实现身份，不能代替 provider namespace。provider ID 是不透明字符串，不能
要求所有 provider 都使用数字 ID。Lux 主程序只实现 metadata RPC v1，不编译上游 provider 的 HTTP client。

metadata 与 `login_background` 插件启动时只通过 `LUX_PLUGIN_CONFIG_PATH` 获得自己的配置文件路径；该进程不会收到
`LUX_CONFIG_DIR`。缺少专属配置文件时，插件应使用自身默认值。`LUX_CONFIG_DIR` 只适用于仍有明确兼容
需求的非 metadata、非登录背景插件，不能被这两类插件依赖。

`type` 当前允许 `metadata`、`media_probe`、`ip_location`、`strm_resolver`、`chapter_detector`、
`data_migration`、`danmaku` 和 `login_background`。媒体探测插件必须同时声明
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
- `emby.route`：由插件 manifest 的 `embyRoutes` 声明精确的 Emby 方法和路径。宿主只向已安装、已启用且
  声明匹配路由的插件转发请求；请求只包含方法、路径、原始 query、脱敏后的非认证头和有界 Base64 body。
  插件返回状态码、受限头和有界 Base64 body。认证头、Cookie、完整 URL 和 Lux 内部身份不会传给插件。
  首个兼容路由是 `POST /Items/SyncMediaInfo`；神医兼容插件在宿主声明 `media.info.import` 时可返回受限的
  `mediaInfoImport` Bundle，宿主负责校验目标并把媒体流、格式、时长、码率和章节写入已索引的 STRM 媒体源。
  未安装或未启用插件时返回 404；插件或 Bundle 不满足能力和大小边界时拒绝请求。该路由不触发远程媒体读取或
  SHA1 计算，SHA1、FF 缓存和远程缓存仍由 MediaTidy 负责。
- `ip.location`：接收一个已由 Lux 宿主校验的公网 IP，返回统一的归属地字段；第三方供应商协议只存在于插件进程。
- `chapters.detect`：接收同一季度至少两个分集的有界 Chromaprint 指纹序列。每个分集只包含请求内临时 `key`、固定 `sampleRate: 11025`、`fingerprintPointDurationTicks: 1238095`、指纹 Base64、窗口起点和窗口时长；宿主对每个文件固定取第一个音频流并让 FFmpeg chromaprint muxer 输出 raw `uint32` 点序列，按 little-endian 编码，不能把 Base64 字节索引当作时间。插件不得接收路径、URL、媒体源 ID 或任务对象。结果只能返回 `IntroStart`、`IntroEnd` 和 `CreditsStart`，时间必须落在对应窗口内，置信度为 0-1。
- `chapters.lookup`：接收请求内临时 `key`、TMDb/TVDb/IMDb ID、季号、集号和可选时长；插件不得接收路径、URL、媒体源 ID、音频指纹或任务对象。插件只能访问 manifest 声明的固定网络主机，返回 `IntroStart`、`IntroEnd` 和 `CreditsStart`；无数据时返回空标记，宿主不会因空响应删除已有标记。
- `migration.test`：接收管理员提交的 Emby 基础地址、API key 和明确的局域网访问许可，返回脱敏服务器信息及
  `historyCapability`。较新的插件可以额外返回可选布尔值 `supportsFilteredReads: true`，表示插件能够在发起
  Emby 请求前执行宿主传入的用户、来源库和字段投影；缺省或 `false` 表示不支持。基础地址不得含凭据、查询参数或片段；
  API key 不得出现在结果、日志或错误消息中。
- `migration.list_users`：宿主可传入有界的 `startIndex`、`limit` 和可选 `search`，插件应在 Emby 用户查询阶段执行
  分页和名称筛选，并在结果中返回可选的 `startIndex`、`totalRecordCount`、`nextStartIndex` 元数据。插件调用 Emby 用户
  接口并返回用户名、显示名、启用状态、管理员标记、远程访问/内容下载策略、媒体库文件夹策略和头像标签；同时返回
  Emby 虚拟媒体库文件夹的稳定 ID、名称和位置，用于将受限用户的库权限映射到 Lux；不返回 Emby token、密码或完整
  原始响应。支持过滤的插件可接收可选 `userIds` 数组，只读取这些用户；未提供时保持列出全部用户的兼容行为。旧插件
  忽略分页字段并返回完整列表时，宿主会在本地执行搜索和切页。支持过滤的插件还可接收可选 `userFields` 字符串数组，
  宿主在执行迁移时始终请求 `id`、`name`，并按范围追加 `hasPassword`、`isDisabled`、`enableRemoteAccess`、
  `enableContentDownloading`（用户资料）或 `enableAllFolders`、`enabledFolders`、`libraryFolders`（媒体库权限/媒体
  状态）。插件必须在发起 Emby 用户请求前应用字段投影；未请求的字段不得读取。配置页的用户预览不使用该投影，以保留
  完整摘要；旧插件不接收 `userFields`，继续按原有完整读取语义工作。
- `migration.list_items`：接收一个 Emby 用户 ID 和有界分页参数，返回 Movie、Series、Season、Episode 的稳定 ID、
  ProviderIds、层级字段和可选的用户状态；未知条目类型不得被伪造成支持类型。
- `migration.user_state`：接收一个 Emby 用户 ID、有界分页参数和必填的 `stateFilter`；筛选值为
  `PLAYED`、`FAVORITE` 或 `RESUMABLE`，分别只返回已看、收藏或存在可继续播放位置的媒体。支持过滤的插件还会收到可选
  `stateFields`（分别为 `played`/`playCount`/`lastPlayedDate`、`isFavorite` 或 `playbackPositionTicks`）以及
  `sourceLibraryIds`，插件必须在 Emby 请求前应用这些投影和来源虚拟库限制。若一次迁移选择多个筛选项，宿主会在每个
  筛选请求中发送所选字段的并集，以便按 Emby 条目 ID 去重时不丢失其他已选字段；未选择的字段仍不会发送或写入。
  宿主应分别请求三种筛选并按 Emby 条目 ID 去重；
  空的已确认来源库范围表示无需发起该用户的状态请求。当前 `historyCapability: "ITEM_STATE"` 只表示条目聚合状态，不得生成假的历史事件。
- `migration.authenticate_user`：仅在 Lux 用户首次登录迁移账户时接收一次用户名和密码，向 Emby 验证后只返回成功及
  脱敏用户身份；插件不得返回或持久化 Emby access token、密码或完整认证响应。
- `login_background.get`：使用空对象参数获取登录页背景数据；不得接收媒体库 ID、媒体标题、路径、URL、用户数据或任务对象。
- 迁移插件必须声明 `type: "data_migration"`、`category: "MIGRATION"` 和 `capabilities: ["migration.emby"]`。
  宿主负责迁移任务、映射、导入、幂等、恢复和历史事件落库；插件不能访问 Lux 数据库、媒体目录或任务对象。
- `plugin.shutdown`：请求插件优雅退出。

配置字段支持 text、password、select、toggle、number 和 `login_background` 专用的 `image`；select 可通过 multiple: true 声明多选，选项使用
`{ "value": "...", "label": "..." }`。`number` 可以声明 `minimum`、`maximum` 和
`defaultValue`。select 可以声明 `optionsSource`，当前支持 `media-libraries`，由 Lux 根据当前
媒体库动态填充选项，不把媒体库 ID 或路径写死在插件包中。管理 API 返回的 `configValues` 只允许包含非敏感当前值。
`image` 字段必须可选、非敏感且无 defaultValue，且只允许 `org.lux.login-background` 在 `login_background` manifest 中声明一个。它表示 Lux 宿主托管的单个图片资产，不是普通文件路径或图片 URL 输入框。其配置值只能是 `sha256:<64 位小写十六进制>` opaque asset ID；只通过管理员图片上传 API 创建/替换，插件不会收到本地路径或图片字节。
媒体库动态填充选项，不把媒体库 ID 或路径写死在插件包中。片头片尾插件不得用 `libraryIds` 配置媒体库归属；
媒体库通过 Lux API 的 `chapterSourceId` 选择数据源。管理 API 返回的 `configValues` 只允许包含非敏感当前值。

### 登录页背景数据合同（API v1）

登录背景提供者必须声明 `type: "login_background"`、`category: "UTILITY"` 和唯一能力
`login_background.get`。该能力不能附加到 metadata 刮削器或其他插件类型。manifest 用
`permissions.imageHosts` 单独声明浏览器将直接请求的 HTTPS 图片域名；它与插件进程的
`permissions.network` 出站主机列表用途不同。图片主机必须是精确 DNS 域名，不支持通配符、IP
字面量、本地域名或端口。

版本化 manifest 示例：

```json
{
  "formatVersion": 1,
  "id": "org.lux.example-login-background",
  "name": "Example login background",
  "version": "1.0.0",
  "apiVersion": 1,
  "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
  "type": "login_background",
  "category": "UTILITY",
  "capabilities": ["login_background.get"],
  "permissions": {
    "network": ["api.example.com", "commons.wikimedia.org", "creativecommons.org"],
    "imageHosts": ["images.example.com", "thumb.wikimedia.org"],
    "filesystem": []
  },
  "files": []
}
```

宿主以空对象作为方法参数调用 `login_background.get`。其 `result` 示例：

```json
{
  "contentKind": "POSTER_FEED",
  "sourceName": "Example catalog",
  "copyrightNotice": "Example catalog image attribution",
  "items": [{
    "imageUrl": "https://images.example.com/posters/example.jpg",
    "title": "Example film",
    "copyrightNotice": "© Example rights holder"
  }]
}
```

每日单图来源可返回 `SINGLE_IMAGE`，署名与许可证链接必须由插件列入 `permissions.network`：

```json
{
  "contentKind": "SINGLE_IMAGE",
  "sourceName": "Wikimedia Commons · Picture of the Day",
  "items": [{
    "imageUrl": "https://thumb.wikimedia.org/example.jpg",
    "title": "Example photograph",
    "copyrightNotice": "By Example Photographer · CC BY-SA 4.0",
    "attributionUrl": "https://commons.wikimedia.org/wiki/File:Example.jpg",
    "licenseUrl": "https://creativecommons.org/licenses/by-sa/4.0/"
  }]
}
```

`contentKind` 只允许 `POSTER_FEED`（0–40 项）、`HERO_IMAGE`（恰好 1 项）、`SINGLE_POSTER` 或 `SINGLE_IMAGE`（后两者恰好 1 项）。响应 JSON 最大
256 KiB，每条图片 URL 最大 2048 字节。图片 URL 通常必须是无凭据、无片段的 HTTPS URL，且主机必须与
`permissions.imageHosts` 中某一域名精确匹配；拒绝 localhost、本地域名和 IP 字面量。唯一例外是
`org.lux.login-background` manifest 声明 `type: "image"` 配置字段时，可返回精确固定路径
`/api/v1/auth/login-background/custom-image`。该能力不接受其他相对路径、查询参数或前缀匹配；Lux
仅在该插件为当前启用来源且 `source` 为 `CUSTOM_IMAGE` 时公开服务校验后的资产。未知字段、
超量条目、含控制字符或超长的署名文本会被拒绝。`sourceName` 为必填纯文本；版权字段及每项
`title`/`copyrightNotice` 为可选纯文本。每项也可带 `attributionUrl` 和 `licenseUrl`：两者必须为
HTTPS、无凭据/片段/端口的 URL，主机必须与 `permissions.network` 中某一域名精确匹配；宿主只按文本链接显示，永不将其作为服务端请求目标。
空 `POSTER_FEED` 不代表可展示内容，宿主应按背景来源的
降级策略处理。`SINGLE_POSTER` 用于原比例完整呈现一张海报；`SINGLE_IMAGE` 用于原比例完整呈现带署名的单张图片。两种单图布局均不得裁切、旋转、拼贴、压暗或在图像上叠加遮罩；署名与许可链接作为独立宿主文字呈现。插件只能提供数据，所有文字呈现、海报瀑布流、图像布局和鸣谢交互均由 Lux 实现。

所选登录背景插件只由 Lux 后台 worker 调用：首次选择、安装/启用或配置更新后会异步刷新，之后至少每日检查一次；失败采用有界退避。Lux 持久化经校验的响应 JSON（每个插件缓存条目不超过 256 KiB），不下载或复制图片字节；登录页公开接口仅读缓存，缓存超过 48 小时、插件未安装/启用/可用或刷新失败且无有效缓存时回退固定背景。刷新期间仍可使用 48 小时内的旧缓存。管理员可见稳定的来源状态，但不会收到插件内部错误详情。插件卸载时其缓存随安装记录一并删除。

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
详情中的 `BelongsToCollection.Id` 可返回 JSON 整数或字符串；Lux 会将其规范化为字符串 ID。此兼容
不改变 `ProviderIds` 映射值必须为字符串的约定。

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

媒体探测 RPC 只能由后台 STRM 探测任务执行，不得在播放、PlaybackInfo 或普通用户请求处理器中同步调用。PlaybackInfo 与媒体浏览不触发探测；仅 LUX-840 条件满足时，实际播放可排入有界的单来源后台任务。Lux 宿主负责并发、超时、取消、重启恢复、任务状态、数据库写入和可选的
`*-mediainfo.json` 原子写入。写入已有 sidecar 时只更新宿主管理的媒体信息字段，并保留未知字段、章节及外部扩展；`existingInfoPolicy=OVERWRITE`
表示重新探测并更新这些宿主字段，不表示删除未知字段。已有 sidecar 无法解析时不得覆盖原文件。Lux 不生成或修改 `.strm` 内容，也不在该 RPC 中计算 SHA1。
`permissions.network` 是能力声明，不替代宿主的出站 URL 安全策略。

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
    "data": {
      "libraryId": "library-1",
      "addedCount": 1,
      "source": "lux",
      "title": "媒体新增",
      "content": "新增媒体：1 个\n媒体库：library-1",
      "body": "新增媒体：1 个\n媒体库：library-1",
      "timestamp": "2023-11-14T22:13:20Z"
    }
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

通知器把需要管理员填写的凭据声明为唯一的 `secret` 配置字段：

```json
{
  "key": "secret",
  "label": "设备密钥",
  "type": "password",
  "required": true,
  "sensitive": true,
  "description": "在 Bark App 首页查看"
}
```

约束与宿主语义：

- 只有 `type: "notification"` 插件可以声明 `secret` 字段，且最多一个；该字段必须是 `sensitive: true` 的
  `password` 字段，不能声明 `defaultValue`。
- 声明后，创建通知目标必须由管理员提交 `secret`，更新时留空表示不修改，`rotate-secret` 用请求体里的新值
  覆盖旧值。宿主不会为第三方渠道生成随机凭据。
- 该值走通知目标 secret 存储：不进入 `config`/`providerConfig`，不出现在普通列表、事件 payload、投递记录或
  日志中，只在创建和轮换的响应里返回一次。
- 凭据属于一个 provider：更换通知器时宿主不会沿用旧凭据。新 provider 声明了 `secret` 字段时必须在同一次更新
  里重新提交，否则返回校验错误；切换到 `builtin.webhook` 由宿主重新生成 HMAC secret；切换到未声明该字段的
  provider 时旧值会被清除。
- 通知器的其它 `configFields` 属于**每条通知目标**，不是插件安装级配置；`required` 由创建/更新目标时校验，
  未创建任何目标不会让 provider 变成不可用。除 `secret` 外，通知器不得把任何字段声明为 `sensitive: true` 或
  `type: "password"`：这些字段走非秘密 `providerConfig`，声明成秘密字段会承诺宿主并未提供的保护。
- 目标 `providerConfig` 由宿主按 manifest 的 `configFields` 校验：未知键、缺少 required 值、类型不匹配、
  非法选项和超出范围的数值都在创建/更新时返回 400，不会原样转发给插件。
- 宿主把凭据声明通过 `GET /api/v1/admin/notification-providers` 的 `secretField` 元数据交给 Web 控制台，
  插件自身不需要读取任何配置文件。

### 通知器 SDK

`luxd::application::notifier_plugin_sdk` 提供通知插件共用的进程外壳，渠道插件只需实现上游认证和请求：

- `run_notifier(|request| async { ... })`：读取 stdin 的逐行 `PluginRequest`，把 `notification.send` 分发到
  处理器，写出 `PluginResponse`，并在 `plugin.shutdown` 后退出；无效请求行和未知方法返回错误响应但不会
  终止循环。需要自定义流时使用 `run_notifier_session(reader, writer, handler)`。
- `outbound_http_client(proxy_url)` / `outbound_http_client_from_env()`：统一的出站 client，读取 Lux 代理配置
  （`LUX_PROXY_URL`），10 秒超时且不跟随重定向。
- `classify_http_status(status)` 与 `result_from_http_status(status, retry_after_seconds)`：与宿主一致的
  `DELIVERED`/`RETRYABLE`/`FAILED` 分类（`2xx` 成功，`408`、`425`、`429` 和 `5xx` 可重试，其余永久失败）；
  `delivered()`、`retryable(code)`、`failed(code)` 用于非 HTTP 渠道。
- `render_event_text(&event)`：返回中文 `(title, body)`。优先原样转发核心生成的 `data.title`/`data.content`
  （插件不得重写通知正文）；缺失时按事件白名单渲染回退文本，且不会带出远程地址、本地路径或完整外部 URL。

插件二进制建议只做如下接线：

```rust
let client = luxd::application::notifier_plugin_sdk::outbound_http_client_from_env()?;
luxd::application::notifier_plugin_sdk::run_notifier(|request| {
    let client = client.clone();
    async move { deliver(&client, request).await }
})
.await?;
```

通知器插件包使用 `scripts/package-notifier-plugin.sh <插件目录名> [target]` 打包：脚本从
`plugins/<目录名>/manifest.json` 读取 ID、版本、入口和声明文件，拒绝非 `notification` 或缺少
`notification.send` 的 manifest，构建入口对应的 cargo bin，并输出
`dist/plugins/<id>-<version>-<platform>-<arch>.zip`。

插件必须返回 `DELIVERED`、`RETRYABLE` 或 `FAILED`，可选返回 provider 请求 ID、重试秒数和稳定错误码：

```json
{"status":"DELIVERED","providerRequestId":"message-1"}
```

宿主负责超时、重试、退避、投递记录和失败恢复。通知插件进程不会获得 `LUX_CONFIG_DIR`，不能通过文件系统
读取其他插件或服务器 Secret。Webhook、Telegram、企业微信等平台的 payload 和认证逻辑属于各自插件，不应
写入通知核心。

Lux 核心在事件进入投递队列前统一生成 `source`、`title`、`content`、`body` 和 ISO `timestamp`，并将它们
放入 `event.data`。播放事件的进度条、播放方式、设备、大小、码率、远程 IP 和简介等展示文本也由 Lux 核心
生成；通知插件不得重新生成或覆盖这些字段。`org.lux.webhook` 的 `url` 配置可以引用 `{title}`、`{content}`
等核心字段并进行 URL 编码，但这只影响目标地址，不改变通知正文。插件可以根据目标平台增加外层字段映射，
但必须保留 Lux 提供的统一通知内容。

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

## 企业微信应用通知插件

`org.lux.notifier.wecom-app` 通过企业微信自建应用发送文本通知，二进制为
`lux-plugin-notifier-wecom-app`，源码在 `src/bin/lux-plugin-notifier-wecom-app.rs`。插件只做渠道认证和
payload 组装，进程外壳、出站 client、状态分类和通知正文来自
[通知器 SDK](#通知器-sdk)。

`configFields` 都是**每条通知目标**的非敏感设置，凭据只通过唯一的 `secret` 字段下发：

```json
{
  "id": "org.lux.notifier.wecom-app",
  "type": "notification",
  "category": "NOTIFICATION",
  "capabilities": ["notification.send"],
  "configFields": [
    {"key": "corpId", "label": "企业 ID", "type": "text", "required": true},
    {"key": "agentId", "label": "应用 AgentId", "type": "number", "required": true, "minimum": 1, "maximum": 2147483647},
    {"key": "toUser", "label": "接收成员", "type": "text", "required": false, "defaultValue": "@all"},
    {"key": "apiBase", "label": "API 基地址", "type": "text", "required": false, "defaultValue": "https://qyapi.weixin.qq.com"},
    {"key": "secret", "label": "应用 Secret", "type": "password", "required": true, "sensitive": true}
  ]
}
```

`secret` 是自建应用的 corpsecret，只从 `notification.send` 请求的 `NotificationSendRpcRequest.secret`
取得，不读取配置文件、不写入 `config`/`providerConfig`，也不出现在日志、RPC 结果或错误信息里。`apiBase`
仅用于代理或测试，必须是 http/https 基地址，且不得包含凭据、query 或 fragment；基地址里已有的路径段
（包括百分号转义）按原样保留，插件只在其后追加 `cgi-bin/*` 路径段。插件进程的出站请求仍受宿主代理策略
（`LUX_PROXY_URL`）约束，沿用 SDK client 的 10 秒超时且不跟随重定向。

投递流程：

1. `GET {apiBase}/cgi-bin/gettoken?corpid=&corpsecret=` 换取 `access_token`。token 缓存在插件进程内，key 为
   `corpId + agentId + secret` 的哈希，到期前 5 分钟刷新；缓存用 `Mutex` 保护且不跨 `.await` 持锁。
2. `POST {apiBase}/cgi-bin/message/send?access_token=` 发送 `msgtype: "text"` 消息，正文为 SDK
   `render_event_text` 返回的标题与正文（`标题\n\n正文`），超过企业微信 2048 字节限制时按 UTF-8 字符边界截断。
3. 上游 `errcode` 为 `40001`/`40014`/`42001` 时清除缓存 token、重新获取后重试一次；再次失败返回
   `Failed(token_rejected)`。`-1`（系统繁忙）与 `45009`（频率限制）返回 `Retryable`，有 `Retry-After` 时填充
   `retry_after_seconds`；其他非 0 返回 `Failed(upstream_errcode_<n>)`。

重试预算：整次投递（含 token 刷新重试路径）有一个 25 秒的总截止时间，小于宿主的 30 秒通知 RPC 超时；单次
上游请求另有 6 秒截止时间，因此最坏情况（两次尝试各一次 gettoken 加一次 message/send）为 24 秒，仍在总预算
内。总截止时间耗尽时按“消息是否可能已经发出”分类：已经发出过 `message/send` 时返回
`Failed(timeout_after_send)`，避免宿主重试造成重复通知；尚未发出任何消息时返回
`Retryable(delivery_timeout)`，可以安全重试。单次 `message/send` 请求自身超时仍按 `Retryable(network_error)`
返回，宿主按至少一次语义投递并负责去重。

收件人判定：企业微信返回 `errcode: 0` 后，插件把 `invaliduser`、`invalidparty`、`invalidtag` 和
`unlicenseduser` 与配置的 `toUser` 逐项比对。全部收件人都无效时返回 `Failed(no_valid_recipient)`；部分无效
仍然 `DELIVERED`，只在 stderr 记录无效数量，不记录 userid。`toUser` 为 `@all` 时无法逐项枚举，按企业微信的
`errcode: 0` 视为已投递。

稳定结果：

- `DELIVERED`：企业微信返回 `errcode: 0`，`msgid` 作为 `providerRequestId` 返回。
- `RETRYABLE`：网络错误/超时（`network_error`）、`408`/`425`/`429`/`5xx`（`PROVIDER_HTTP_<status>`）、
  `rate_limited`、`system_busy`，或总截止时间内尚未发出任何消息（`delivery_timeout`）。
- `FAILED`：缺少或空白凭据（`missing_secret`）、目标配置非法（`invalid_config`，如 corpId 为空、agentId
  非正数、toUser 含非法字符、apiBase 非 http/https 或带凭据/query）、全部收件人无效
  （`no_valid_recipient`）、已发出消息后超出总截止时间（`timeout_after_send`）、响应超 64 KiB
  （`response_too_large`）、响应不是预期 JSON（`invalid_response`）、其他永久 4xx 与业务错误。

插件日志只写稳定结果码和无效收件人数量，不写 access_token、token 请求的 query、完整外部 URL、userid 或凭据。

打包：`scripts/package-notifier-plugin.sh org.lux.notifier.wecom-app`，产物为
`dist/plugins/org.lux.notifier.wecom-app-0.1.0-<platform>-<arch>.zip`。相关测试在
`tests/plugins/notifier_wecom_app.rs`，使用 127.0.0.1 随机端口的 mock 企业微信服务，不访问真实租户。
