# Lux Plugin SDK v1

## 目标

Lux 插件是放在 `/config/plugins` 中、由服务重启后发现的独立插件包。首版使用 `.zip` 包格式和独立进程运行时。插件不直接访问 Lux 数据库、媒体根目录或内部任务对象。

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
└── signature.json
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
  "supportedItemTypes": ["Movie", "Series", "Season", "Episode", "Person", "BoxSet"],
  "capabilities": [
    "metadata.search",
    "metadata.details",
    "metadata.images",
    "metadata.externalIds",
    "metadata.trailers"
  ],
  "configFields": [],
  "permissions": {
    "network": [],
    "filesystem": ["plugin-cache"]
  },
  "files": [],
  "signature": {
    "algorithm": "ed25519",
    "keyId": "lux-official",
    "value": "..."
  }
}
```

`formatVersion` 是包格式；`apiVersion` 是 RPC 契约；`version` 是插件自身版本。三者不能混用。

正式包必须使用 Ed25519 签名。Lux 从 `/config/plugins/trusted_keys.json` 读取公钥，格式为
`{"keyId":"base64-encoded-32-byte-public-key"}`；也可以用同样 JSON 内容设置
`LUX_PLUGIN_TRUSTED_KEYS`。没有匹配受信 key 的包不会被发现或启动。私钥只允许通过发布环境的
`LUX_PLUGIN_SIGNING_KEY_HEX` 传入，不能写进仓库或 manifest。

构建官方 TMDb 包：

```bash
LUX_PLUGIN_KEY_ID=lux-official \\
LUX_PLUGIN_SIGNING_KEY_HEX=<32-byte-ed25519-private-key-hex> \\
./scripts/package-tmdb-plugin.sh
```

脚本输出 `org.lux.tmdb-<version>.zip`，包含 `manifest.json`、`signature.json` 和当前平台的
`binaries/<platform>-<arch>/lux-plugin-tmdb`。插件目录只在 Lux 重启时扫描，升级包后需要重启。

## RPC 方法

消息使用 JSON-RPC 风格的 `id`、`method`、`params`、`result` 和 `error` 字段，通过插件进程 stdin/stdout 传输。插件 stdout 只允许输出协议消息，诊断日志写 stderr。

方法：

- `plugin.hello`：协商协议、返回 manifest 摘要和运行状态。
- `plugin.health`：返回插件是否可用，不返回凭据。
- `metadata.search`：返回 Emby 风格 `RemoteSearchResult` 列表。
- `metadata.get`：返回 `MetadataResult<T>`。
- `metadata.images`：返回 `RemoteImageInfo` 列表。
- `metadata.externalIds`：返回 `ProviderIds`。
- `metadata.trailers`：返回预告片候选。
- `plugin.shutdown`：请求插件优雅退出。

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
