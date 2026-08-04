# ADR-010：动态插件包与独立进程运行时

## 状态

已接受。

## 背景

Lux 需要把 Emby `MovieDb.dll` 的 TMDb 行为重写成可独立升级的 Lux 插件，并允许未来的第三方插件放入插件目录后由服务重启发现。原始 `MovieDb.dll` 是依赖 Emby 4.8.11 API 的 .NET 托管程序集，不能直接作为 Rust native DLL 加载；让任意第三方代码注入 Lux 主进程还会把插件崩溃、任意文件访问和供应链风险扩大到整个服务。

## 决策

### 插件包

插件发布格式使用常规 ZIP 扩展名 `.zip`，例如 `org.lux.tmdb-1.0.0.zip`。包根目录必须包含 `manifest.json`，并可包含 `binaries/`、`assets/` 和 `signature.json`。manifest 声明稳定插件 ID、SemVer 版本、协议版本、运行时入口、平台架构、能力、配置字段、权限和文件哈希。

Lux 在 `/config/plugins` 扫描 `.zip` 或开发用解压目录。包不会因为文件名存在就运行：必须通过路径、manifest、格式版本、协议版本、平台、哈希和签名校验。正式插件要求受信任签发者；首版不从任意远程 URL 自动下载包。

签名使用 Ed25519。受信公钥由 `/config/plugins/trusted_keys.json` 或
`LUX_PLUGIN_TRUSTED_KEYS` 提供；签名覆盖去掉 `signature` 字段后的规范化 manifest，manifest
中的 `files` 列表再覆盖包内可执行文件的 SHA-256。发布器使用 `lux-plugin-pack` 生成签名，私钥
只从发布环境参数读取。

### 运行时

插件默认以独立子进程运行。Lux 负责启动、stdin/stdout RPC 通信、超时、取消、健康检查、崩溃记录和重启；插件不能获得 Lux SQLite、内部任务对象或媒体根目录的直接引用。插件使用声明的能力访问网络和自己的缓存目录，元数据写回、图片下载和 Emby API 输出由 Lux 完成。

协议采用 JSON-RPC 风格的请求 ID 和稳定错误码，方法包括 `plugin.hello`、`plugin.health`、`metadata.search`、`metadata.get`、`metadata.images`、`metadata.externalIds`、`metadata.trailers` 和 `plugin.shutdown`。协议的公开数据模型使用 Emby 风格名字，包括 `BaseItem`、`Movie`、`Series`、`Season`、`Episode`、`Person`、`BoxSet`、`MetadataResult`、`RemoteSearchResult`、`RemoteImageInfo`、`ProviderIds` 和 `ImageType`；Lux 内部仍保留独立领域模型。

### TMDb

`org.lux.tmdb` 是独立 Lux 插件，不加载原始 `MovieDb.dll`。它按反编译得到的行为重写电影、剧集、季、集、人物、合集、图片、外部 ID、预告片、语言、缓存、限流、重试和 TMDb v3 API Key 逻辑。已有内置 TMDb 客户端代码迁移为可复用客户端库，插件进程负责 RPC 适配。

### 生命周期

插件目录是包文件的来源，SQLite 只保存发现、启用、版本、哈希、状态和错误。首版服务重启时扫描并生效，不做热加载；插件 ID 是媒体库 `scraperId` 的稳定值，版本升级不改变媒体库选择。

## 替代方案

### 在 Rust 主进程中直接 `dlopen` native DLL

拒绝：原始 DLL 是 .NET 程序集，不是稳定 C ABI；任意 native 插件还可以破坏主进程内存，跨平台需要分别维护 `.dll`、`.so` 和 `.dylib`。

### 直接运行原始 Emby `MovieDb.dll`

拒绝作为主路径：它依赖 `MediaBrowser.Controller`、`MediaBrowser.Model`、`MediaBrowser.Common` 以及 Emby 的依赖注入、实体、HTTP、文件系统、缓存和 Provider 生命周期。模拟完整 Emby 运行时的成本高于重写 TMDb 行为。

### WASM 插件

暂不采用：隔离性更好，但现有 TMDb 行为需要重新适配 WASI、网络、图片和缓存；未来可以作为新的 runtime kind，不改变当前 RPC 契约。

## 后果

- 插件可以独立发布和升级，Lux 重启后从 `/config/plugins` 发现。
- 插件崩溃不会直接终止 Lux 主进程，但同一操作系统用户下的子进程仍需要部署权限隔离，不能把进程监督误认为完整沙箱。
- `org.lux.tmdb` 不再是 Rust 主进程内置实现；原始 Emby DLL 只作为行为参考。
- 需要维护插件包格式、RPC 协议、签名信任和跨平台运行时矩阵。
