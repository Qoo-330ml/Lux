# LUX-051：TMDb 客户端边界实施计划

## 范围

提供可替换的 TMDb HTTP client boundary：读取 API Read Access Token、统一 Bearer 认证、请求超时、受控并发间隔、429/5xx/网络超时重试与指数退避抖动、响应大小/schema 校验，以及 zh-CN 缺字段到 en-US 的逐字段回退。

## 实现

- [x] `TmdbClientConfig` 支持 `LUX_TMDB_READ_ACCESS_TOKEN`、base URL、超时、重试和请求速率配置。
- [x] 支持电影搜索、电影详情、Bearer token、429/5xx/超时分类与重试。
- [x] 限制响应体大小并验证分页、电影 ID 和详情 ID。
- [x] zh-CN 搜索结果缺字段时按匹配的 TMDb ID 请求 en-US 并逐字段补缺。
- [x] 本地 Axum stub 覆盖 200、404、429、5xx、超时、坏响应和认证 header；不调用真实 TMDb。

## 官方边界依据

- 认证：[TMDb Application Authentication](https://developer.themoviedb.org/docs/authentication-application)
- 搜索：[TMDb Movie Search](https://developer.themoviedb.org/reference/search-movie)
- 限流：[TMDb Rate Limiting](https://developer.themoviedb.org/docs/rate-limiting)
- 详情：[TMDb Movie Details](https://developer.themoviedb.org/reference/movie-details)

## 明确不做

- 不把 token 返回到 API、日志或 fixture。
- 不在本阶段实现候选评分、待处理管理 API 或真实网络集成；分别属于 LUX-052/LUX-053。
