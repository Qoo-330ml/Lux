# LUX-090：媒体库 ACL 审计记录

## 审计范围

- Lux 与 Emby 的列表、详情、搜索、图片、字幕、播放、下载和合集成员端点。
- 通过已知 item ID、media source ID 和 image ID 访问资源。
- 普通用户跨库访问、同库可见但无下载权限两种边界。

## 结果

- [x] 列表、详情和搜索只返回用户可见媒体库。
- [x] 图片、字幕、播放和 source-specific 播放先执行 item/source ACL。
- [x] 下载先执行 item ACL，再检查 can_download；无库权限不会泄露下载策略。
- [x] BOX_SET 详情和成员列表按集合所属库及成员所属库过滤。
- [x] ACL 集成测试覆盖 Lux/Emby 已知 ID、source ID、image ID 和搜索入口。
