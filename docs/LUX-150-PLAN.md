# LUX-150：Lux 弹幕兼容插件与后台匹配

## 状态

已实现，待真实支持弹幕接口的 Emby 客户端兼容性验证。

## 目标

把原 Emby 弹幕插件的实用能力重写为 Lux 内置服务：管理员配置一个自定义弹幕 API 基地址，Lux 在后台为媒体库中的视频匹配弹幕，并将成功结果原子地写回视频同目录、同文件名的 `.xml` 旁车。Lux 同时提供与已知 Emby 弹幕插件约定兼容的读取端点，使支持弹幕接口的第三方客户端可以通过 Emby 连接使用已匹配弹幕。

首版承诺的是 XML 弹幕和 Dandanplay 兼容协议，不承诺普通 Emby 字幕端点能让不支持弹幕协议的客户端显示弹幕。

## 已确认的产品决定

- 自定义地址支持 Dandanplay 兼容 API 基地址。
- 自定义地址也支持 [`huangxd-/danmu_api`](https://github.com/huangxd-/danmu_api) 的 API 基地址。该项目兼容 Dandanplay 的搜索、详情和弹幕获取接口，并额外提供 `POST /api/v2/match`。
- 基地址可以包含部署 token 路径，例如 `https://danmu.example/87654321`；Lux 必须保留该路径，不能把 token 写入日志或错误消息。
- 插件配置持久化允许匹配的媒体库 ID，以及原始文件名、简繁标题、英文/原始标题三个匹配开关；空媒体库选择表示不匹配任何库。
- 标题候选来自已索引的媒体标题和原始标题，Lux 不在请求路径调用 TMDb 或执行中文字符转换；上游 `danmu_api` 的简繁转换继续由其部署环境变量管理。
- 匹配成功后写入视频旁边的同名 `.xml`，例如 `Episode 01.mkv` 对应 `Episode 01.xml`。
- 只承诺支持弹幕接口的客户端；其他软件是否识别 `.xml` 属于客户端能力，不增加兼容层或转码兜底。
- 不实现 Web 播放器弹幕、不实现 ASS 写回、不实现 Lux 侧弹幕文字转换、不做代理播放、不实现弹幕发送或实时发布。

## 外部协议适配

### 输入地址

管理员通过 Lux 设置保存一个非空的 HTTP/HTTPS 基地址。Lux 在保存时规范化末尾 `/`，保留路径前缀，并拒绝凭据、fragment、控制字符和不支持的 scheme。客户端请求不能提交或覆盖这个地址。

请求上游时只使用固定的相对接口路径和受限参数：

- `POST /api/v2/match`，请求体至少包含 `{ "fileName": "..." }`。Lux 可以按配置顺序用多个候选文件名分别请求；这是 `danmu_api` 的自动匹配接口；成功响应中的 `matches[0].episodeId`、`animeId` 和标题用于后续取弹幕。
- `GET /api/v2/search/anime?keyword=...`、`GET /api/v2/search/episodes?...` 和 `GET /api/v2/bangumi/{id}`，作为 Dandanplay 兼容搜索/详情回退。
- `GET /api/v2/comment/{episodeId}?format=xml`，取得 Bilibili 标准 XML。

实现不得假设所有 Dandanplay 兼容服务都支持 `match`。上游返回 404/405 或明确表示接口不存在时，按本地解析的标题、年份、季号和集号走搜索/详情/弹幕读取回退；其他 4xx、5xx、超时和响应格式错误进入任务失败，不无限重试。

上游响应必须满足大小上限、Content-Type/格式和 XML 结构校验；XML 至少包含标准弹幕根节点和弹幕节点，不能把任意 HTML 或 JSON 写入 `.xml`。

### Lux 兼容读取接口

Emby 路由保持独立 DTO 和鉴权边界，新增只读兼容端点：

- `GET /api/danmu/{itemId}`：返回当前默认媒体源的 XML 地址/兼容结果。
- `GET /api/danmu/{itemId}/raw`：返回 XML 原文，`Content-Type` 为 `application/xml; charset=utf-8`。
- `GET /api/danmu/{itemId}?option=Refresh`：仅刷新 Lux 已登记的旁车索引，不在客户端请求中访问上游、不启动整库任务。
- `GET /api/danmu/{itemId}?option=GetJsonById`：保留已知 Emby 弹幕插件的兼容别名；实现只返回 Lux 已有的弹幕记录，不把 XML 强行转换成未经验证的 JSON。

实际客户端调用序列以兼容性测试为准；未被测试的 Emby 弹幕私有端点不属于首版承诺。

## 本地旁车与数据模型

### 旁车规则

- 仅处理已索引的本地视频来源；`.strm` 不参与弹幕旁车写回。
- 旁车路径由已校验媒体路径派生，必须位于同一媒体库根路径内。
- 目标文件为视频 basename 替换扩展名后的 `.xml`，大小上限由配置常量约束。
- 写入使用同目录临时文件、刷盘和原子 rename；失败不得留下半个目标 XML。
- 已存在的旁车默认不覆盖；管理员任务可以显式传 `overwrite=true`。
- 旁车删除或校验失败时，索引记录标记为缺失/失败，不删除媒体条目。

### 新表

`danmaku_tracks` 关联 `media_sources`，保存 `media_source_id`、`relative_path`、`format`、`provider`、`provider_anime_id`、`provider_episode_id`、内容 fingerprint、状态和时间戳。路径只保存相对媒体根路径，查询时再次执行根路径约束。

`danmaku_match_jobs` 保存管理员发起的媒体库任务、`overwrite`、并发上限、状态、计数、错误和时间戳；`danmaku_match_job_items` 保存每个媒体源的排队、匹配、写回、失败或跳过状态及脱敏错误码。任务项以媒体源和任务去重，服务重启后 `RUNNING` 项回到 `PENDING`。

迁移必须可以从空数据库执行，并为新表建立媒体库、媒体源、任务状态和更新时间索引。

## 后台任务

管理员通过 `POST /api/v1/admin/libraries/{libraryId}/danmaku/match` 创建任务，请求至少支持：

- `overwrite`：是否覆盖已有 XML，默认 false。
- `concurrency`：任务并发，使用服务端上限校验。

任务提供列表、详情、取消和失败重试接口；所有列表分页且有服务器端上限。任务运行时：

1. 选择该媒体库已索引的本地视频源，不做请求路径全库扫描。
2. 对每个视频生成受限的 filename 查询，优先调用 `/api/v2/match`。
3. 取得匹配 episode 的 XML，校验后原子写回旁车。
4. 记录 provider ID、fingerprint 和结果状态；单个媒体失败不终止整项任务。
5. 遇到取消请求时不再领取新任务，已在进行的上游请求在超时或取消边界结束后记账。

默认重试只覆盖临时网络失败；无匹配、XML 无效、根路径不可写和权限错误需要管理员显式重试或文件变化后再处理。

## API 契约

Lux 自有管理接口增加：

- `GET/PATCH /api/v1/admin/settings` 中的 `danmaku` 配置对象，响应只返回脱敏地址和是否已配置，不回显 URL 中的 token/query secret。
- `GET/PUT /api/v1/admin/plugins/{pluginId}/config` 的弹幕插件配置包含 `providerBaseUrl`、`libraryIds`、`matchOriginalFilename`、`matchSimplifiedTraditionalTitles` 和 `matchEnglishTitle`；敏感地址不回显。
- `POST /api/v1/admin/libraries/{libraryId}/danmaku/match`
- `GET /api/v1/admin/danmaku/match-jobs`
- `GET /api/v1/admin/danmaku/match-jobs/{jobId}`
- `POST /api/v1/admin/danmaku/match-jobs/{jobId}/cancel`
- `POST /api/v1/admin/danmaku/match-jobs/{jobId}/retry`

管理接口使用现有管理员鉴权和 CSRF/审计规则。媒体读取接口使用现有 Emby token、用户和媒体库 ACL；任何用户都不能通过 `itemId` 读取其他媒体库的旁车。

## 验收标准

- [x] 空数据库迁移成功；已有同名有效 XML 可以在扫描后的媒体源上被登记并读取。
- [x] 管理员可保存/清除 Dandanplay 兼容地址和 `danmu_api` 地址；包含路径 token 的地址请求正确，日志和错误不泄露 token。
- [x] `/api/v2/match` 成功响应可以匹配单集；不支持 `match` 时搜索/详情回退可工作；匹配失败不会写入 XML。
- [x] 成功 XML 写入视频旁边的同名 `.xml`，写入中断不会留下损坏目标文件；默认不覆盖已有 XML。
- [ ] Emby 兼容读取端点返回正确 XML、执行 ACL，并可被支持弹幕接口的客户端使用。（接口与 ACL 已测，真实客户端待测）
- [x] 后台任务支持分页、进度、取消、失败重试和重启恢复；并发不超过配置上限。
- [x] 未配置地址、无匹配、上游超时、非 XML、超大响应、旁车越权路径和只读目录都有稳定错误状态。
- [ ] 管理员可以选择弹幕媒体库和标题候选匹配开关；未选媒体库不能创建任务；匹配候选按配置顺序回退。
- [x] 不新增 Web 播放器弹幕、ASS、转码、实时发送或非弹幕客户端适配。

## 预计实施切片

按以下顺序保持每个增量可编译、可回滚：

1. 数据库迁移、旁车路径/ XML 校验、已有旁车登记和单元/存储测试。
2. 上游 Dandanplay/`danmu_api` 客户端与脱敏 URL/响应契约测试。
3. 持久后台任务、恢复/取消/重试和原子 XML 写回测试。
4. Lux 管理 API、Emby 兼容读取端点、ACL 和协议测试。
5. API 文档、兼容性记录、ARM 检查和全量项目检查。

每一切片完成后单独提交；LUX-150 完成后在进入其他增强前等待项目所有者确认。

## 验证命令

```bash
uname -m
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
./scripts/check-all.sh
```

真实客户端兼容性还需记录至少一个支持弹幕接口的 Emby 客户端版本；标准 Emby 字幕客户端不作为弹幕验收对象。
