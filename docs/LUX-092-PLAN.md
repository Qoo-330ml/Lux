# LUX-092：转发客户端 IP 和远程访问行为实施记录

## 规则

- Lux 始终优先读取 X-Forwarded-For 的第一个有效地址；没有有效转发头时回退到 TCP peer。
- 生产服务使用 Axum ConnectInfo 注入 TCP peer，覆盖并清理外部伪造的内部 peer 标记。
- 远程访问不再依据 IP 或 can_remote_access 判断，认证和媒体库 ACL 仍然有效。

## 认证和媒体

- Web 登录、Web session、Emby 登录和 Emby token 仍执行账号认证。
- 受保护媒体端点复用认证和媒体库 ACL；用户不能绕过账号或媒体库权限浏览详情、图片、字幕、播放或下载。
- 已创建的 session/token 仍不能绕过认证和媒体库 ACL。

## 验证

- [x] 无代理 CIDR 配置时仍读取转发客户端 IP。
- [x] 没有有效转发头时回退 TCP peer。
- [x] 转发 HTTPS 协议可以标记 Secure Cookie。
- [x] 转发场景下认证和媒体库 ACL 测试覆盖。
