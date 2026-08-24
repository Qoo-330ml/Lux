# LUX-194：演员搜索与人物参演作品实施计划

## 范围

用户在 Web 搜索中输入演员姓名时，可以看到人物结果并进入人物详情；人物详情分页展示当前用户
有权限访问的全部参演电影和剧集。分集关系聚合到所属剧集，同一剧集只展示一次。

## API 合同

- `GET /api/v1/people?q={query}&page={page}&pageSize={pageSize}`：返回分页演员摘要。
- `GET /api/v1/people/{personId}/items?page={page}&pageSize={pageSize}`：返回分页媒体条目。
- 结果统一使用 Lux 分页字段 `items`、`total`、`page`、`pageSize`。
- 两个接口均使用当前会话的媒体库权限；空权限返回空列表，不泄露人物或媒体存在性。

## 实现切片

1. 扩展 catalog 过滤边界，支持按持久化人物关系筛选可见 `MOVIE`/`SERIES`，并验证分集聚合语义。
2. 增加人物姓名搜索存储查询和 PeopleService 方法，按 provider/person ID 去重。
3. 增加两个 Lux API 路由和 Rust 集成测试。
4. 增加 API client、搜索页人物结果、人物详情作品列表和 Web 测试。
5. 运行 Playwright 关键流程及 Rust/Web 全量检查。

## 明确不做

- 不调用 TMDb 或其他在线服务搜索演员。
- 不增加导演、编剧、制作公司搜索。
- 不修改 Emby `/Persons`、`/Search/Hints` 的响应合同。
- 不一次性加载全部作品；“全部”通过服务端分页和前端继续加载完成。
