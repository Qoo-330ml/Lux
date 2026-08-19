# LUX-144：TMDb 多语言与 API 地址配置

## 目标

为 `org.lux.tmdb` 提供可持久化的首选语言、语言回退、标题别名替换和替代 API 地址设置。TMDb 插件使用首选语言请求电影、剧集、季度和单集元数据；开启回退后，按管理员选择的语言顺序逐字段补全空值。开启标题别名替换后，当中文首选语言返回的标题不含中文字符时，插件从 TMDb `alternative_titles` 接口选择第一个 `CN` 中文别名替换标题。替代 API 地址开关默认关闭，开启后可选择官方地址、`https://api.tmdb.org` 或填写自定义 HTTP(S) 基础地址。

## 行为契约

- 首选语言默认是 `zh-CN`，界面选项来自 TMDb 当前主翻译语言列表，并按 `zh-CN`、`zh-SG`、`zh-HK`、`zh-TW`、其他语言排序。
- 语言回退默认关闭，但默认选中的回退语言为 `zh-SG`、`zh-HK`、`zh-TW`，并保持用户选择的顺序。
- 回退只补全电影、剧集、季度和单集元数据中的空字符串、空数组或 null；已有非空字段不被覆盖。
- 标题别名替换默认关闭；开启后仅对中文首选语言生效，电影和剧集标题没有中文字符时，尝试使用 `alternative_titles` 返回的第一个有效 `CN` 别名，接口失败时保留原标题。
- 非敏感设置写入 `/config/tmdb_settings.json`；API 只返回设置值和 schema，不返回 API Key 或 Read Access Token。
- 修改设置后重启 TMDb 插件进程使新设置对后续请求生效。
- 替代 API 地址默认关闭并使用 `https://api.themoviedb.org`；开启后插件请求优先使用保存的地址，地址不得包含凭据、查询参数或片段。

## 实施切片

1. 扩展插件配置 schema，增加 select/toggle、多选选项和非敏感 `configValues`。
2. 增加 TMDb 设置文件读写、边界校验和管理员配置 API。
3. 在独立 TMDb 插件中应用首选语言及电影/剧集/季/集逐字段回退。
4. 在 Web 插件配置对话框中加入首选语言下拉、回退开关和多选下拉。
5. 在 Web 插件配置对话框中加入标题别名替换开关、替代 API 地址开关、预设地址下拉和自定义地址输入，并让独立插件使用该配置。

## 验证

- Rust 插件协议、管理员配置和独立进程回退集成测试。
- Web 插件配置组件和 API client 测试。
- `cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`pnpm --dir web test`、`pnpm --dir web build`。

## 来源

TMDb 将元数据语言表示为带地区的 IETF 标签，并提供主翻译语言列表接口：

- https://developer.themoviedb.org/docs/languages
- https://developer.themoviedb.org/reference/configuration-primary-translations
