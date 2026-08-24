# ADR-024：Provider-neutral 元数据刮削器

## 状态

已接受

## 背景

Lux 当前同时支持内置/外置 TMDb 刮削路径和 provider-neutral 的插件 RPC，但部分应用服务仍通过
`TmdbPluginClient`、TMDb 数字 ID、`Tmdb` provider 名称和 TMDb 图片 URL 处理通用元数据。这样会使
IMDb、豆瓣或其他刮削器必须伪装成 TMDb，且可能在非数字 provider ID、图片、人物和合集路径中产生错误。

Lux 后续需要支持多个元数据来源。插件实现身份、元数据 provider namespace、provider ID 格式和能力
集合必须能够独立变化；现有 TMDb 行为和数据库兼容性不能因此回归。

## 决策

1. 应用层以 provider-neutral metadata contract 为唯一业务边界，统一使用 `ScraperSearchResult`、
   `ScraperMetadata`、`ScraperMetadataBundle`、`ScraperImagesResponse` 和相关通用模型。
2. TMDb endpoint façade 和 `Tmdb*` typed DTO 只保留在 TMDb adapter 内部；通用插件直接调用
   `metadata.search/get/bundle/images/credits/externalIds/trailers` RPC。
3. metadata 插件声明稳定的 `providerKey`。`pluginId` 只表示安装/运行时实现身份，不能作为媒体
   provider namespace 的替代品；旧插件缺少该字段时仅在兼容期间从插件 ID 推导。
4. provider ID 在应用层按字符串处理，并以 provider key 命名空间定位。业务代码不得调用
   `first_provider_id()` 作为当前 provider 的身份，也不得通过 `tmdb` 字符串特判选择 ID。
5. provider capability 由 manifest 和通用 capability 查询决定。业务层不能假设所有 provider 都支持
   BoxSet、图片、预告片或季度/单集。
6. 保持现有 `provider_ids_json` 数据库存储和 Lux 对外 API；namespace 和 ID 的强类型转换在应用层
   边界完成，不新增 provider 专用表或 migration。

## 未采用的方案

### 让 IMDb/豆瓣实现 TMDb DTO

这会减少初始改动，但要求所有 provider 使用 TMDb 数字 ID、图片路径和 collection 语义，无法表达
`tt...`、`nm...` 或 provider 特有能力，最终会把 provider 差异重新泄漏到业务层。拒绝。

### 仅通过插件 ID 最后一段推导 provider key

这对 `org.lux.tmdb` 和 `org.lux.imdb` 有效，但插件发布者可能使用不同的实现 ID；它只作为旧 manifest
兼容策略，不作为新插件的正式合同。

### 为每个 provider 增加独立数据库关系

这会使新增 provider 需要 migration 和重复查询路径。现有 JSON provider ID 存储已经足够，应用层强类型
namespace 可以在不改变数据库结构的情况下提供边界安全。

## 后果

- TMDb adapter 需要保留并测试现有语言回退、合集和图片 URL 转换。
- 候选、重新识别、图片、人物和合集服务需要从 TMDb 名称和类型改为通用 provider 名称。
- 插件 manifest 和 SDK 文档需要增加 provider key 及 capability 约束。
- 新增 IMDb、豆瓣测试 fixture 后，可以在不修改业务服务的情况下实现对应插件。
- 对不支持的能力必须返回稳定的 unavailable/not-supported 结果，不能回退到 TMDb 或伪造字段。

## 验证

- TMDb 现有 Rust 测试全部通过。
- provider-neutral fixture 同时覆盖数字 ID、`tt/nm` ID 和任意字符串 ID。
- 选中 provider 与多个外部 ID 并存时，候选和 NFO 只使用选中的 provider ID。
- 图片、人物和合集 capability 缺失时不发起错误的 TMDb 请求。
