# LUX-102 至 LUX-106：管理控制台阶段记录

## 已完成的基础纵切片

- [x] 管理入口只对 canManageServer 用户显示。
- [x] 控制台显示 ready/schema、媒体库数量、用户数量和最近审计。
- [x] 用户创建、禁用操作连接真实管理 API，并发送 CSRF。
- [x] 服务端错误以可读状态展示，不留下空白页面。
- [x] 媒体库创建、根路径添加、实时监听/扫描计划保存和手动扫描入口连接真实 API。
- [x] 用户创建、改密、禁用/恢复、服务器权限、远程/下载权限和媒体库 ACL 在控制台可操作。
- [x] 管理页展示 pending 候选字段差异，并支持“仅补缺/刷新未锁定”选择和写回。
- [x] 管理页分页读取扫描任务，支持按状态过滤、取消和重试。
- [x] 媒体库可停用/启用，停用后普通用户不可见且不能新建扫描；管理员可删除媒体库根路径配置（不删除媒体文件）。
- [x] 管理员可删除媒体库配置和索引数据；服务端拒绝删除仍有扫描任务的媒体库，并在事务中清理库级计划任务配置。
- [x] 任务列表支持查看单任务详情，包括状态、进度、游标、代次和结构化错误。
- [x] 管理员可读取条目图片索引信息，并安全删除媒体根目录内的图片及索引；路径越界和符号链接会拒绝操作。
- [x] 新增管理员健康诊断 API `/api/v1/admin/health` 和脱敏日志入口 `/api/v1/admin/logs`；健康响应包含 schema、配置/ffprobe/TMDb、库根和活动任务状态，不返回配置或媒体绝对路径。
- [x] `scripts/admin-smoke.mjs` 固化管理员登录、创建媒体库、添加/删除根路径、发起扫描、查看任务详情、停用和删除媒体库流程；多用户普通用户权限由 `scripts/browser-smoke.mjs` 覆盖。
- [x] Chrome smoke 已验证初始化后的登录和管理入口权限边界；管理员和普通用户流程已分别固化为可重复脚本。

## 后续切片

- [x] 任务日志详情、结构化错误筛选和更长历史分页；新增 `scan_job_events` 迁移、管理员分页 API 和控制台筛选。
- [x] 新候选搜索；管理员可按标题/年份搜索 TMDb 并保存 pending 候选。
- [x] 批量重新识别任务；新增持久化任务/条目模型、管理员创建/状态查询/失败重试 API、队列 worker、条目级稳定错误代码和管理页批量选择及最近任务状态入口。

验证：`cargo test --locked --test reidentify --test libraries_api --test users --test images` 覆盖批量任务、媒体库/任务管理和图片列举删除；`node --check web/src/app.mjs` 通过。

管理员脚本运行示例：
`LUX_E2E_BASE_URL=http://127.0.0.1:18506 LUX_E2E_ADMIN_USERNAME=admin LUX_E2E_ADMIN_PASSWORD='…' LUX_E2E_MEDIA_ROOT='/media' NODE_PATH='<bundled-node-modules>' node scripts/admin-smoke.mjs`
