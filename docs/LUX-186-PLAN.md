# LUX-186：插件商店更新检查与安全更新

## Objective

为 Lux 管理员插件页面增加更新检查和单插件更新能力，解决已安装插件长期停留在旧 ZIP、页面无法识别
商店新版本的问题。更新必须复用现有插件商店的目录限制和包校验边界，并保留插件配置与启用状态。

## API contract

- `GET /api/v1/admin/plugins?page=1&pageSize=50`
  - 已安装且本地 manifest 可发现的插件返回 `version`（当前版本）。
  - 若当前商店目录存在对应条目，返回 `latestVersion`；`updateAvailable` 表示商店版本高于当前版本。
  - 此请求重新读取商店目录，前端“检查更新”通过重新请求该接口完成。
- `POST /api/v1/admin/plugins/{pluginId}/update`
  - 仅管理员可调用，使用现有 Cookie CSRF/API Key 管理员鉴权。
  - 成功返回 `{ "plugin": <PluginView> }`。
  - 无更新返回 HTTP 409、错误码 `PLUGIN_NO_UPDATE`。
  - 请求体为空；下载地址、版本和 SHA-256 只来自当前商店目录。

## Implementation files

- `docs/LUX-DEVELOPMENT.md`：登记 LUX-186 范围和验收。
- `src/application/plugins.rs`：版本比较、更新状态、保留配置的插件包更新服务。
- `src/api/mod.rs`：更新路由、管理员鉴权、审计事件和错误映射。
- `tests/plugins.rs`：版本状态、无更新和管理员更新 API 合同测试。
- `web/src/lib/api/types.ts`：更新状态字段。
- `web/src/lib/api/client.ts`：更新请求客户端。
- `web/src/features/admin/AdminPluginsPage.tsx`：检查更新和更新操作。
- `web/tests/plugin-library.test.ts`：前端更新状态、按钮和检查行为测试。

## Boundaries

- 不增加数据库表或迁移；配置和 `installed_plugins` 状态保持原样。
- 不支持请求指定远程地址、包版本或校验值。
- 不把卸载再安装作为更新流程；卸载仍保持删除配置的原有语义。
- 不在更新接口中运行 STRM 探测任务；更新后只同步已有的插件计划任务配置。

## Verification

- Rust 专项测试、格式化、Clippy。
- Web 单测和构建。
- 真实浏览器验证插件页面的按钮、状态、无障碍名称、网络方法/路径和控制台。
- `uname -m` 记录为本机验证环境，不宣称 NAS 性能。
