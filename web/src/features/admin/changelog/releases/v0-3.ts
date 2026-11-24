import type { ChangelogRelease } from "../types";

export const changelogReleases03: ChangelogRelease[] = [
  {
    version: "0.3.12",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 媒体库卡片带数量",
        "✨ feat: 局域网设备配对",
        "✨ feat(scan): 旁车文件边扫边登记",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 首页不再吐旧快照",
        "🐛 fix(scan): 旁车登记跟上扫描进度",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 服务器概况重排",
        "♻️ refactor(auth): 配对边界收紧了",
      ] },
    ],
  },
  {
    version: "0.3.11",
    date: "2026-09-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): TMDb 加了「原语言」模式",
        "✨ feat(playback): 继续观看能看出是哪部剧",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 原语言图片按语言挑",
        "♻️ refactor(media): 分集带上剧集名",
      ] },
    ],
  },
  {
    version: "0.3.10",
    date: "2026-09-08",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 后台任务能看到在跑什么",
        "✨ feat(metadata): TMDb 语言按语言组展示",
        "✨ feat(plugin): 自定义执行计划能删了",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(build): 计划配置不再重复建计划",
        "🐛 fix(scan): 任务列表标签准了",
        "🐛 fix(web): 仪表盘去掉重复面板",
        "🐛 fix(media): 剧集在「最新」里会挪位置",
      ] },
      { kind: "changed", items: [
        "⚡ perf(scan): 扫描索引瘦身",
        "♻️ refactor(api): 任务活动接口统一带当前媒体",
      ] },
    ],
  },
  {
    version: "0.3.9",
    date: "2026-09-08",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 执行计划按任务类型管",
        "✨ feat(subtitle): MKV 播放和字幕补齐",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(subtitle): 字幕竞态修好了",
        "🐛 fix(subtitle): 解析加了边界和上限",
        "🐛 fix(plugin): 插件同步不再覆盖自定义计划",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(api): 任务的展示层级改了",
        "♻️ refactor(scan): 实时扫描",
      ] },
    ],
  },
  {
    version: "0.3.8",
    date: "2026-09-04",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 兼容删除接口",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 平铺电影目录的 NFO",
        "🐛 fix(emby): Hills 等客户端的兼容",
        "🐛 fix(playback): .strm 播放鉴权",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(playback): .strm 改用短期票据",
      ] },
    ],
  },
  {
    version: "0.3.7",
    date: "2026-09-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): 管理员用户分页查询",
        "✨ feat(emby): Emby 条目 ID 改数字",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 扫描不再留过期记录",
        "🐛 fix(scan): 扫描与 .strm 修复",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 重启不自动接续任务",
        "♻️ refactor(playback): .strm 入口统一了",
      ] },
    ],
  },
  {
    version: "0.3.6",
    date: "2026-09-01",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 用户管理补齐",
        "✨ feat(emby): 同季度上下集",
        "✨ feat(auth): 用户配置会存下来",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(auth): 一批用户相关的问题",
        "🐛 fix(metadata): 人物索引与条目拆分",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(api): 服务器概况和 DTO 收敛",
      ] },
    ],
  },
  {
    version: "0.3.5",
    date: "2026-08-31",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(api): 隐私浏览也能写请求",
        "🐛 fix(emby): 弹幕与 Emby 字段兼容",
        "🐛 fix(scan): 文件短暂缺失不再被删",
      ] },
      { kind: "changed", items: [
        "⚡ perf(danmaku): 弹幕匹配和扫库并发可调",
        "♻️ refactor(scan): 扫描不容易被卡住",
      ] },
    ],
  },
  {
    version: "0.3.4",
    date: "2026-08-31",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(playback): 远程 .strm 播放",
        "🐛 fix(metadata): NFO 评分和用户接口",
        "🐛 fix(api): HTML 响应和 CSRF",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 章节标记并进共享时间轴",
        "⚡ perf(metadata): 批量预读与稳定推荐",
      ] },
    ],
  },
  {
    version: "0.3.3",
    date: "2026-08-30",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 移动端详情页布局",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 移动端详情页与几处播放问题",
        "🐛 fix(playback): 远程 .strm 与容器识别",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(image): 移动端海报和操作区",
      ] },
    ],
  },
  {
    version: "0.3.2",
    date: "2026-08-30",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 迁移能圈范围",
        "✨ feat(web): 首页和播放入口的调整",
        "✨ feat(scan): 扫描后处理有进度了",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(scan): 全量扫描期间的几处问题",
        "🐛 fix(subtitle): 一批兼容问题",
      ] },
      { kind: "changed", items: [
        "⚡ perf(db): 迁移批处理更快",
        "♻️ refactor(db): 移动端控制与迁移向导",
      ] },
    ],
  },
  {
    version: "0.3.1",
    date: "2026-08-28",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): LuxPlayer 播放器成型",
        "✨ feat(db): 剧集级删除和连接池",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(playback): 一批播放与鉴权问题",
        "🐛 fix(playback): 混合媒体库与播放票据",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 播放页换成新播放器",
        "♻️ refactor(danmaku): 章节、字幕、弹幕跟着媒体源走",
      ] },
    ],
  },
  {
    version: "0.3.0",
    date: "2026-08-27",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 元数据能接别的供应商",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 迁移与刮削的一批问题",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(plugin): TMDb 和豆瓣搬进插件",
      ] },
    ],
  },
];
