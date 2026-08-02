# LUX-101：初始化向导实施记录

- [x] 读取 setup/status，未初始化时只显示管理员创建表单。
- [x] 调用 setup/complete 创建首个管理员，成功后引导登录。
- [x] 初始化完成后服务端 setup API 仍保持一次性关闭语义。
- [x] 初始化表单支持可选 TMDb Read Access Token；服务端只写入配置目录 `tmdb_read_access_token`，Unix 权限为 `0600`，响应不返回 token。
- [x] 初始化表单支持创建首个媒体库、可选根路径，或留空跳过；根路径仍经过可用性和可读性检查。

验证：`cargo test --locked --test setup`（4 项）、`cargo test --locked --test web_static`、`node --check web/src/app.mjs` 和 Web 请求选项单测通过。

说明：TMDb 配置在服务启动时从环境变量 `LUX_TMDB_READ_ACCESS_TOKEN` 优先读取，否则回退到配置目录文件；初始化后若新增 token，重启服务即可让后台 TMDb 任务加载该 token。
