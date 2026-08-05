# LUX-057：统一媒体文件名解析与 Movie/TV 元数据匹配实施计划

## 目标

将 qmby 已验证的媒体文件名解析思路收敛为 Lux 应用层共享能力，清洗文件名和目录名中的技术噪声，补齐年份与季集信息，并让 Lux 按媒体库所选刮削器对电影/剧集使用正确的元数据搜索接口。

## 依赖关系

```text
media_matching parser
        ├── scanner title/year/season/episode
        ├── candidate query + Movie/Series route
        ├── reidentify query + Movie/Series route
        └── selected scraper metadata.search
```

## 实施切片

### 1. 共享解析器

- 文件：`src/application/media_matching.rs`、`src/application/mod.rs`、`tests/media_matching.rs`
- 验收：统一解析 `暗夜与黎明2024`、`S01E01`、`2x07`、中文季集标记和技术标签；标题、年份、季集号和版本字段稳定。
- 验证：解析单元测试先失败，再实现后通过。

### 2. 扫描器接入

- 文件：`src/application/scanner.rs`、`tests/scanner.rs`、`tests/series_scanner.rs`
- 验收：剧集目录标题使用清洗结果和年份；单集标题不含编码/来源噪声；原有层级 identity 和媒体源版本字段保持稳定。
- 验证：scanner/series_scanner 集成测试。

### 3. Lux 候选与元数据匹配分流

- 文件：`src/application/candidates.rs`、`src/application/reidentify.rs`、`src/application/tmdb.rs`、测试文件
- 验收：候选搜索使用条目所属媒体库的刮削器；刮削器返回多个 provider ID 时只保存所选刮削器的 ID；TMDb 刮削器的 Movie/Series 继续分别走 `/search/movie` 和 `/search/tv`。
- 验证：本地 Axum 刮削器 stub 检查 provider ID、路径、查询参数和候选结果。

### 4. 独立 TMDb 插件复用

- 文件：`src/bin/lux-plugin-tmdb.rs`、`tests/tmdb_plugin.rs`
- 验收：插件 `metadata.search` 复用统一清洗，Movie/Series 分流和现有 Emby 风格响应保持兼容。
- 验证：standalone plugin RPC 集成测试。

## 边界和风险

- 本任务不改变插件 JSON-RPC 的公开字段，不把真实 `.strm` URL 或凭据写入日志。
- 在线元数据请求仍只能在后台任务或插件进程中调用；解析器本身是纯函数。
- 本任务生成或存储候选，不自动选择低置信候选覆盖本地元数据；自动确认仍遵守 LUX-052 的保守门槛。
- 本机验证记录 `uname -m`；不据此宣称 NAS/x86 性能。
