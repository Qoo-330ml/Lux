# ADR-028：Provider 实现与 Lux 主程序解耦

## 状态

已接受

## 日期

2026-08-27

## 背景

Lux 已有 provider-neutral 的 metadata RPC，但主程序仍编译 TMDb HTTP client、typed endpoint DTO、
凭据优先级和 TMDb adapter；同时 metadata 插件进程可以继承整个 `LUX_CONFIG_DIR`。这使新增豆瓣、IMDb
或其他 provider 仍需要理解主程序的实现细节，也扩大了插件看到无关服务器凭据的范围。

## 决策

1. 主程序只保留 `ScraperProvider`、`ScraperPluginClient`、manifest/catalog 和 metadata RPC v1 通用模型。
   上游 HTTP client、DTO、认证、回退语言、图片 URL 处理和 provider 特定能力属于插件仓库。
2. metadata manifest 必须声明稳定的 `providerKey`，可以声明通用 `aliases`。`pluginId` 不作为 provider
   namespace；旧插件缺字段时只在兼容期从 ID 最后一段推导 provider key。
3. provider ID 在主程序中按不透明字符串处理。`tmdb`、`douban`、`imdb` 仅作为数据和协议 namespace，
   通过通用兼容表处理，不触发 provider-specific client 或数字转换。
4. 宿主为 metadata 插件传递 `LUX_PLUGIN_CONFIG_PATH`，值是该插件的受限配置文件；metadata 插件不继承
   `LUX_CONFIG_DIR`。配置文件由宿主按 manifest 字段写入，敏感字段仍只在插件进程使用。
5. 旧 TMDb 共享文件只在首次发现时做一次幂等复制到
   `plugin-config/org.lux.tmdb.json`，并写入迁移完成标记。成功后保留旧文件用于回滚/一个发布周期；这条
   迁移路径不创建 provider client、不发起上游请求，标记写入后主程序不再读取这些字段的上游含义。

## 未采用方案

### 保留一个“仅测试”内置 TMDb client

这仍会让主程序编译第三方 endpoint、凭据和图片规则，测试也会继续把实现边界固定在 TMDb。测试改用
provider-neutral fake adapter 或独立插件进程。

### 让插件继续继承整个配置根目录

这无法证明插件隔离，且一个被攻破的 metadata 进程可以读取其他插件凭据和服务器 secrets。专属配置路径
是更小且可测试的权限边界。

## 后果

- Lux 核心不再因 TMDb API 变化重新构建；更新 TMDb/豆瓣只发布插件包。
- 插件仓库需要维护自己的上游 client 和配置迁移兼容。
- 旧 NFO/Emby 行为继续存在，但它们属于协议兼容而不是 TMDb 实现依赖。
- 所有 metadata 集成测试必须使用通用 fixture 或独立插件 RPC，不能重新引入 `TmdbClient`。

## 验证

- `rg` 检查 Lux 非测试源码不含 `TmdbClient`、`tmdb_plugin`、TMDb endpoint、运行时凭据解析和图片 CDN
  转换；唯一保留的旧凭据读取位于一次性兼容迁移函数，且只复制到插件专属配置文件。
- runtime 测试证明 metadata 子进程只收到 `LUX_PLUGIN_CONFIG_PATH`。
- 旧配置和 alias 迁移测试、TMDb/豆瓣 RPC v1 测试、Rust/Web 全量检查与性能基准通过。
