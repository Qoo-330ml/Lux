# LUX-100：Web 工程和 API 客户端实施记录

## 已完成

- [x] Rust 同源提供 index.html、app.mjs 和 styles.css，不依赖 CDN。
- [x] Web fetch client 统一 JSON 错误处理、same-origin credentials 和 CSRF header。
- [x] 登录、会话恢复、退出、初始化向导、首页、媒体库入口、搜索、详情和 source 选择直放。
- [x] 响应式布局覆盖窄屏和桌面，表单控件、跳过链接、焦点样式和错误状态具备可访问基础。

## 验证

    node --check web/src/app.mjs
    pnpm --dir web run build
    cargo test --test web_static

## 边界

- 当前 Web 使用浏览器原生 ES module，尚未引入 React/TypeScript 构建链；管理控制台和复杂页面在后续 LUX-102 至 LUX-106 拆分。
