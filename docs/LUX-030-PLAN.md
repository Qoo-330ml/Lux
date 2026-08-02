# LUX-030：媒体库与多根路径模型实施计划

## 范围

实现第一个电影纵切片所需的媒体库和根路径管理能力，不实现扫描、NFO、ffprobe 或 Emby Items。接口使用开发文档规定的管理路径：

- `GET /api/v1/admin/libraries`
- `POST /api/v1/admin/libraries`
- `POST /api/v1/admin/libraries/{libraryId}/roots`

所有接口要求有效 Web 会话，并在服务端检查 `can_manage_server` 与 CSRF。

## 数据与规则

- `libraries` 和 `library_roots` 使用独立 migration，所有时间字段为 SQLite UTC epoch。
- 路径输入先用 `tokio::fs::canonicalize` 规范化，再确认存在、是目录且可读取。
- `isWritable` 独立报告；只读但可读取的媒体根仍可保存，并返回 `LIBRARY_PATH_NOT_WRITABLE` 警告。
- 同一媒体库中相同 canonical path 返回重复错误；同一媒体库中有父子重叠关系返回重叠错误。
- 跨媒体库重叠路径允许保存，但响应返回结构化警告。
- 数据库只保存 canonical path 和用户提交的 display path；不在日志中输出路径。
- 管理 API 响应使用 camelCase，错误使用统一 Lux error envelope。

## 增量任务

### Slice 1：模型、migration、路径检查和 storage

- [x] 新增 `LibraryRootId`、`LibraryKind`、library/root record。
- [x] 新增 `0005_libraries.sql`，建立约束、唯一键和外键。
- [x] 实现目录可用性、可读性、写权限和同库/跨库重叠判定。
- [x] 用临时目录和 SQLite 集成测试验证持久化与重启可读。

### Slice 2：管理员 API

- [x] 实现管理员 Web session + CSRF 授权边界。
- [x] 实现创建/列出媒体库和添加根路径。
- [x] 覆盖未登录、非管理员、CSRF 失败、无效路径、重复/重叠路径和只读警告。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- 本机 `arm64` 上用临时目录创建两个库、添加多个根路径、重启数据库并核对状态。

以上验证已于 2026-08-02 在 `arm64` 本机通过。

## 明确不做

- 不在 API 请求中扫描目录。
- 不读取媒体文件内容。
- 不实现电影发现、NFO、图片、ffprobe 或 Emby Items；这些属于 LUX-031 及以后任务。
