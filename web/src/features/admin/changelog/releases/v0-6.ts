import type { ChangelogRelease } from "../types";

export const changelogReleases06: ChangelogRelease[] = [
  {
    version: "0.6.20",
    date: "2026-10-25",
    sections: [
      { kind: "added", items: [
        "✨ feat(playback): 转码设置独立成页",
        "✨ feat(playback): 转码实时进度",
        "✨ feat(playback): 管理员能停掉转码会话",
        "✨ feat(playback): 转码历史",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(playback): 硬件编码器会自检",
        "♻️ refactor(api): 编码器参数补齐",
        "♻️ refactor(playback): 关掉转码只影响实时播放",
        "♻️ refactor(metadata): 自动合并的界线收紧了",
        "♻️ refactor(metadata): NFO 的读取顺序",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(api): 并发上限对所有请求一致生效了",
        "🐛 fix(playback): 转码汇总不再把软件算成硬件",
        "🐛 fix(web): 浅色主题下的徽章颜色",
      ] },
    ],
  },
  {
    version: "0.6.19",
    date: "2026-10-25",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 播放信息面板",
        "✨ feat(api): 播放统计接口",
        "✨ feat(playback): HLS 会话采集实时进度",
        "✨ feat(web): 看板显示真实输出",
        "✨ feat(plugin): 插件可以不走代理",
        "✨ feat(web): 转码参数可以在后台调",
        "✨ feat(playback): 关掉转码后明确拒绝",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(deploy): 部署级覆盖仍然有效",
      ] },
    ],
  },
  {
    version: "0.6.18",
    date: "2026-10-24",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix: 历史清理按表独立跑",
        "🐛 fix(metadata): 清理不再长时间占着连接",
        "🐛 fix(scan): 孤儿扫描目标会被清掉",
      ] },
    ],
  },
  {
    version: "0.6.17",
    date: "2026-10-24",
    sections: [
      { kind: "added", items: [
        "✨ feat(image): 图片策略可以按格配",
        "✨ feat(media): 分层策略矩阵",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 背景图和艺术图各管各的",
        "⚡ perf(image): 上限在筛选和写入时都生效",
        "♻️ refactor(image): 本地图片始终优先",
        "♻️ refactor(db): 旧配置不用迁移",
        "♻️ refactor: 张数和宽度改成档位",
        "♻️ refactor(image): 图片开关改成两态",
      ] },
    ],
  },
  {
    version: "0.6.16",
    date: "2026-10-23",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 同库的多版本自动合并",
        "✨ feat(web): 合并错了可以拆开",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(media): 升级后已有的重复条目也会合",
        "♻️ refactor(media): 合集详情和媒体详情对齐",
        "♻️ refactor(web): API Key 面板更好认",
        "♻️ refactor(db): 新增一对迁移",
      ] },
    ],
  },
  {
    version: "0.6.15",
    date: "2026-10-23",
    sections: [
      { kind: "added", items: [
        "✨ feat(realtime): 各处预算都有上限",
        "✨ feat(web): 后台任务共用一份预算",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(auth): 认证和访问控制收紧了",
        "♻️ refactor(auth): 外部资源读取收紧",
        "♻️ refactor(emby): 列表更贴近 Emby",
        "♻️ refactor(log): 数据库和日志访问",
        "♻️ refactor(scan): 扫描失败不再连坐",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): HLS 会话的几个安全问题",
        "🐛 fix(scan): 扫描和事件的一串问题",
        "🐛 fix(emby): Emby 列表的问题",
        "🐛 fix(db): 数据库层的杂项",
      ] },
    ],
  },
  {
    version: "0.6.14",
    date: "2026-10-22",
    sections: [
      { kind: "added", items: [
        "✨ feat(auth): 用户管理改成工作区",
      ] },
      { kind: "changed", items: [
        "⚡ perf(scan): 写入压力降低",
        "⚡ perf(db): 数据库性能优化",
        "♻️ refactor(media): 删媒体库改成分批做",
        "♻️ refactor(db): 内容分级索引用持久队列",
        "⚡ perf(web): 后台空闲开销减少",
        "♻️ refactor(db): 旧版兼容修复只跑一次",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): 升级数据库不再中途失败",
      ] },
    ],
  },
  {
    version: "0.6.13",
    date: "2026-10-21",
    sections: [
      { kind: "added", items: [
        "✨ feat: 注册方式可以选",
        "✨ feat(auth): 权限设置更明确",
        "✨ feat(auth): 设备和会话可查",
        "✨ feat(auth): 按用户限制内容分级",
        "✨ feat(web): 删媒体库改成后台任务",
        "✨ feat(playback): STRM 播放时补信息",
      ] },
      { kind: "changed", items: [
        "⚡ perf(web): Web 媒体页更快",
        "♻️ refactor(playback): 黑名单覆盖播放授权",
        "✨ feat(image): 图片缓存改成全局开关",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(api): 改邮箱的并发问题",
        "🐛 fix(realtime): 推送更新少做无用功",
        "🐛 fix(playback): STRM 重试不再绕开限制",
        "🐛 fix(db): 黑名单查询的兼容性",
      ] },
    ],
  },
  {
    version: "0.6.12",
    date: "2026-10-19",
    sections: [
      { kind: "added", items: [
        "✨ feat(auth): 用户管理页能搜能筛",
        "✨ feat(auth): 账号可回收和彻底删除",
        "✨ feat(playback): 按用户限制播放",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(media): 搜索结果混进合集内容",
      ] },
    ],
  },
  {
    version: "0.6.11",
    date: "2026-10-19",
    sections: [
      { kind: "added", items: [
        "✨ feat(scan): 扫描进度看得见",
        "✨ feat(scan): 后处理失败能重试",
        "✨ feat(deploy): 反向代理下的客户端地址",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 自动刮削开关合并",
        "♻️ refactor(web): 首页卡片标题字体",
        "♻️ refactor(web): 详情页标语位置",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(image): 图片读取失败只跳一条",
        "🐛 fix(metadata): 刮削部分成功不重做",
        "🐛 fix(media): 合集从排序里漏掉",
      ] },
    ],
  },
  {
    version: "0.6.10",
    date: "2026-10-18",
    sections: [
      { kind: "added", items: [
        "✨ feat(playback): 继续观看信息更全",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 全量扫描的排队方式",
        "♻️ refactor(plugin): Manifest 查询分批",
        "♻️ refactor(web): 首页标题字体",
        "⚡ perf(db): 媒体合并索引优化",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): 看完一集继续观看会往下走",
      ] },
    ],
  },
  {
    version: "0.6.9",
    date: "2026-10-17",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 后台共用一个 CPU 预算",
        "✨ feat(probe): ffprobe 并发会自动调",
        "✨ feat(scan): 扫描指纹换新格式",
        "✨ feat(probe): STRM 探测队列有门槛",
        "✨ feat(metadata): 自动补信息会退避重试",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 扫描查询少拼 SQL",
        "♻️ refactor(api): 删除差异改成分页",
        "♻️ refactor(scan): 扫描发现分片提交",
        "⚡ perf(probe): 自动并发默认开启",
        "♻️ refactor(web): 活动弹窗只看在跑的任务",
        "♻️ refactor: 自动识别失败原因分得清",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(probe): ffprobe 预算的回收",
        "🐛 fix(metadata): 取消人物任务时文件锁没放",
      ] },
    ],
  },
  {
    version: "0.6.8",
    date: "2026-10-16",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 媒体库网格只渲染看得见的部分",
        "✨ feat(web): 页面靠推送更新",
        "✨ feat(web): 切页面有加载占位",
      ] },
      { kind: "changed", items: [
        "⚡ perf(web): 详情页少做无关更新",
        "♻️ refactor(web): 横向滚动栏更顺",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(auth): 别的登录会话数据不刷新",
        "🐛 fix(emby): Emby 路径的大小写",
      ] },
    ],
  },
  {
    version: "0.6.7",
    date: "2026-10-15",
    sections: [
      { kind: "added", items: [
        "✨ feat(playback): 继续观看条目可以藏起来",
        "✨ feat(image): 子目录里的图也算剧照",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 选完元数据还显示旧数据",
        "🐛 fix(web): 旧前端分块导致白屏",
        "🐛 fix(image): 图像编辑器不再显示旧缓存",
      ] },
    ],
  },
  {
    version: "0.6.6",
    date: "2026-10-15",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 详情页显示 NFO 标语",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 详情页标题样式统一",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): NFO 回写一行一个字段",
        "🐛 fix(metadata): 人物匹配补齐字段",
      ] },
    ],
  },
  {
    version: "0.6.5",
    date: "2026-10-15",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(web): 详情页徽标换位置",
      ] },
    ],
  },
  {
    version: "0.6.4",
    date: "2026-10-15",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 全局电影合集会自动校准",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 详情页信息排版调整",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(media): 最近添加里漏了合集",
      ] },
    ],
  },
  {
    version: "0.6.3",
    date: "2026-10-15",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(db): 迁移 0126 之后升不动",
      ] },
    ],
  },
  {
    version: "0.6.2",
    date: "2026-10-15",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 全局电影合集",
        "✨ feat(scan): 侧车文件变更会被跟踪",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 移动端首页布局调整",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): Emby 认证来源和播放令牌",
        "🐛 fix(scan): 侧车变更没进队列",
      ] },
    ],
  },
  {
    version: "0.6.1",
    date: "2026-10-14",
    sections: [
      { kind: "added", items: [
        "✨ feat(image): 详情页有剧照横滑区了",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 详情页布局调整",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 浅色主题下的卡片显示",
      ] },
    ],
  },
  {
    version: "0.6.0",
    date: "2026-10-14",
    sections: [
      { kind: "added", items: [
        "✨ feat(scan): 扫描进度看得见",
        "📝 docs(docs): 官网和使用指南",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(scan): 补全队列分开排队",
        "♻️ refactor(web): 详情页和首页的排版",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 元数据任务的三处状态错误",
        "🐛 fix(db): SQLite 0126 迁移",
        "🐛 fix(media): 删除媒体源时顺手清理旁车文件",
      ] },
    ],
  },
];
