# ADR-009：内置插件配置与 TMDb 凭据优先级

## 状态

已接受。

## 背景

插件目录需要支持不同插件拥有不同配置项。TMDb 的 Emby 插件使用 v3 API Key，而 Lux 原有客户端使用 v4 Read Access Token；同时，首次引导不应因为在线元数据服务阻塞服务器初始化。

## 决策

内置插件通过非敏感配置 schema 声明自己的配置字段。管理员从插件详情页写入配置，服务端只返回配置是否存在及来源，不返回凭据明文。

TMDb 凭据按以下顺序选择：

1. 管理员保存的 TMDb v3 API Key；
2. 运行时或配置目录中的历史 v4 Read Access Token；
3. Lux 内置的 Emby 兼容默认 API Key。

自定义 API Key 和历史 token 使用配置目录中的受限文件保存，Unix 权限为 0600。清除自定义 API Key 后恢复上述 fallback。插件配置写入不放入 SQLite，也不进入首次引导请求。

## 后果

- 首次启动即可使用 TMDb 插件，不需要用户先完成 TMDb 设置。
- 插件配置 UI 可以按 schema 扩展到其他内置插件，而不修改媒体库模型。
- 内置默认 Key 不能作为用户专属配额或安全凭据；管理员可随时替换为自己的 Key。
- v4 Read Access Token 保留兼容路径，后续可以在独立迁移任务中收敛认证模式。
