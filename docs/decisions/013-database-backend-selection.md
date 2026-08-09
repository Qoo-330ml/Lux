# ADR-013：首次引导选择数据库后端

## Status

Accepted

## Date

2026-08-09

## Context

Lux 默认使用内置 SQLite，以保持单容器 NAS 部署简单。部分部署希望把数据库交给已有的 PostgreSQL 基础设施，以获得更高的并发写入能力、集中备份和数据库运维能力。

数据库选择发生在首次创建管理员之前。此时 Lux 还不能依赖业务数据库保存选择，因此后端配置必须先保存到 `/config` 下的受限启动配置文件。

## Decision

- 首次引导提供 `SQLITE` 和 `POSTGRESQL` 两种后端选择。
- `SQLITE` 使用 `/config/lux.db`，保持现有 WAL、外键和 busy timeout 行为。
- `POSTGRESQL` 使用管理员提供的连接信息；Lux 只测试连接并执行 PostgreSQL migration，不启动 PostgreSQL 服务。
- 后端选择一旦完成首次初始化即固定。当前版本不提供在线切换，也不在启动时自动从一个后端回退到另一个后端。
- PostgreSQL 密码只允许通过受保护的启动配置注入或写入权限受限的配置文件；不得在 API 响应、日志、审计事件或错误详情中返回。
- 数据库配置测试接口只在未初始化状态可用，并且必须遵守现有初始化暴露边界，不能作为公网任意 TCP 探测器。
- Lux API、Emby API 和领域模型不暴露数据库后端细节；管理员健康状态只返回非敏感的后端类型和连接状态。

## Alternatives Considered

### 只支持 SQLite

运维最简单，且符合默认部署，但无法满足已有 PostgreSQL 基础设施的用户。

### 首次引导同时支持 MySQL

不采用第一版。SQLite 到 MySQL 同样需要独立 migration、搜索实现和集成测试，会扩大核心存储层变更面；后续有明确需求时单独评估。

### 在 Lux 容器内启动 PostgreSQL 子进程

不采用。它不是同进程内置数据库，会引入进程监督、数据目录、升级、备份和故障恢复复杂度，并削弱容器单进程边界。

## Consequences

- `storage` 必须拥有稳定的后端无关边界，SQLite 和 PostgreSQL 的 SQL/migration/search 差异只能留在 storage 内。
- 测试默认继续使用临时 SQLite；PostgreSQL 集成测试通过显式环境变量启用，不把本地容器当作所有开发者的必需依赖。
- 现有已初始化 SQLite 实例保持兼容，不受首次引导选项影响。
- 需要更新部署文档，明确 PostgreSQL 的持久化、备份、连接安全和版本兼容要求。
