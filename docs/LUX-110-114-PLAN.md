# LUX-110 至 LUX-114：普通用户 Web 阶段记录

## 已完成

- [x] 登录、会话恢复、退出和首页媒体库入口。
- [x] 媒体库列表支持类型、年份、已看、收藏、名称/最近添加排序。
- [x] 详情页读取播放状态，展示续播信息并支持收藏切换。
- [x] 同源 Web API 支持剧集季度/单集和合集成员，详情页可展开浏览。
- [x] Web 剧集/合集子项返回当前用户 `userData`，详情页支持收藏与已看/未看切换，并要求 CSRF。
- [x] 2026-08-03 本机 Chrome 150 headless smoke：普通用户无管理入口、MP4 直放 metadata readyState=4、stream 返回 206、390px 无横向溢出、控制台/页面错误为 0。
- [x] 浏览器直放和每 10 秒进度上报已连接服务端。
- [x] 账户页展示当前 Web 会话，并允许撤销其他会话；服务端按用户隔离并要求 CSRF。
- [x] 播放器监听 `video.error`，给出明确的“不支持该媒体编码/尝试其他版本或客户端”提示。

## 待完成

- [x] 剧集季度/单集、合集详情和更完整的 UserData 操作。
- [x] 不支持浏览器编码时显示明确错误提示。
- [x] `scripts/browser-smoke.mjs` 支持可播放 MP4 和随机不可播放 `.mp4` fixture；ARM64 Chrome smoke 已验证 `video.error` 明确提示。
- [x] `scripts/browser-smoke.mjs` 固化登录、普通用户无管理入口、直放 metadata、收藏写回、390/768/1440 viewport 横向溢出和基础语义可访问性扫描。

脚本运行示例：
`LUX_E2E_BASE_URL=http://127.0.0.1:18503 LUX_E2E_USERNAME=viewer LUX_E2E_PASSWORD='…' LUX_E2E_LIBRARY=Movies LUX_E2E_ITEM='<item-id>' NODE_PATH='<bundled-node-modules>' node scripts/browser-smoke.mjs`

## 边界

- 直放播放器不会触发服务端转码；浏览器自身不支持的编码仍需显示可诊断提示。
- SenPlayer 的后续媒体请求兼容性按 `docs/COMPATIBILITY.md` 的已知边界记录处理，不作为 Web 验收条件。
