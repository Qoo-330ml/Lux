# LUX-092：可信代理和远近端判断实施记录

## 规则

- 默认没有可信代理，普通 X-Forwarded-For 不影响来源判断。
- 仅当 socket peer 命中 LUX_TRUSTED_PROXY_CIDRS 时，才读取 X-Forwarded-For 的第一个地址。
- loopback、RFC1918、link-local、CGNAT/Tailscale 100.64/10 视为本地；其他有效公网地址视为远程。
- 生产服务使用 Axum ConnectInfo 注入 socket peer，覆盖并清理外部伪造的内部 peer 标记。

## 认证和媒体

- Web 登录成功前、Web session 解析、Emby 登录成功前和 Emby token 解析均检查 can_remote_access。
- 受保护媒体端点复用认证检查；远程禁用用户不能浏览详情、图片、字幕、播放或下载。
- 远程禁用时已创建的 session/token 不会绕过策略。

## 验证

- [x] 不可信 peer 的伪造转发头被忽略。
- [x] 可信代理的公网转发地址被识别为远程。
- [x] Tailscale CGNAT 地址按本地处理。
- [x] 实际 TCP peer + X-Forwarded-For 测试覆盖认证和媒体详情。
