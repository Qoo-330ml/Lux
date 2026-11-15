import type { ChangelogRelease } from "../types";

export const changelogReleases01: ChangelogRelease[] = [
  {
    version: "0.1.18",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 输出的本地 NFO 更全",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): Filmly 分集与主图回退",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(emby): Filmly 播放流配置收窄",
      ] },
    ],
  },
  {
    version: "0.1.17",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 详情页信息更全",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(auth): token 写法和媒体策略",
      ] },
      { kind: "changed", items: [
        "♻️ refactor: Logo 换新",
      ] },
    ],
  },
  {
    version: "0.1.16",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 章节数据能存下来了",
        "✨ feat(metadata): 章节配置和管理入口",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 章节只处理剧集",
      ] },
    ],
  },
  {
    version: "0.1.15",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 本地 NFO 元数据更完整",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 快照和演员信息丢失",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 详情页降级更顺",
      ] },
    ],
  },
  {
    version: "0.1.14",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "✨ feat(image): 元数据对象和图片目录",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): 写回重试和外键迁移",
      ] },
      { kind: "changed", items: [
        "🔧 build(plugin): 插件和构建调整",
      ] },
    ],
  },
  {
    version: "0.1.13",
    date: "2026-08-11",
    sections: [
      { kind: "added", items: [
        "✨ feat(playback): STRM 解析和缩略图",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 层级、首页和缩略图",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(probe): STRM 有变化就只探测它",
      ] },
    ],
  },
  {
    version: "0.1.12",
    date: "2026-08-11",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(probe): STRM 探测结果缺信息",
      ] },
    ],
  },
  {
    version: "0.1.11",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "✨ feat(probe): STRM 定时探测",
      ] },
    ],
  },
  {
    version: "0.1.10",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(deploy): Docker 镜像少文件",
      ] },
      { kind: "changed", items: [
        "🔧 build(build): 构建缓存调了一遍",
      ] },
    ],
  },
  {
    version: "0.1.9",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 任务记录显示所属媒体库",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 排序偏好不生效",
      ] },
      { kind: "changed", items: [
        "⚡ perf(api): 并发和分页加了边界",
      ] },
    ],
  },
  {
    version: "0.1.8",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(probe): 图片命名和探测释放",
      ] },
      { kind: "changed", items: [
        "⚡ perf(db): 查询改成有界批量",
      ] },
    ],
  },
  {
    version: "0.1.7",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(metadata): 候选失败分类等三处",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 元数据后台处理",
      ] },
    ],
  },
  {
    version: "0.1.6",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix: 横向媒体栏箭头跟手了",
      ] },
      { kind: "changed", items: [
        "⚡ perf(scan): 扫描性能记录补全",
      ] },
    ],
  },
  {
    version: "0.1.5",
    date: "2026-08-09",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(auth): 只有版本号变动",
      ] },
    ],
  },
  {
    version: "0.1.4",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): Redia Emby 兼容回退",
      ] },
    ],
  },
  {
    version: "0.1.3",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): 可选的 PostgreSQL 部署",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): PostgreSQL 路径和几处页面问题",
      ] },
    ],
  },
  {
    version: "0.1.2",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): 首次引导选数据库",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): PostgreSQL 下的几个问题",
      ] },
    ],
  },
  {
    version: "0.1.1",
    date: "2026-08-08",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(metadata): 元数据刷新失败不再连累别人",
      ] },
    ],
  },
  {
    version: "0.1.0",
    date: "2026-08-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(core): Rust 模块化单体服务",
        "✨ feat(db): 媒体库和索引",
        "✨ feat(playback): 播放和个人数据",
        "✨ feat(web): Lux Web 端和管理台",
        "✨ feat(deploy): 部署与验证",
      ] },
    ],
  },
];
