import type { ChangelogRelease } from "../types";

export const changelogReleases01: ChangelogRelease[] = [
  {
    version: "0.1.18",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "✨ feat(emby): Emby 输出的本地 NFO 更全 — 走 Emby 接口生成的本地 NFO 元数据补全了，Filmly 的媒体流和图片字段也一起补上。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(emby): Filmly 分集与主图回退 — 分集列表、next-up、season DTO、媒体流和主图回退这几处都修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(emby): Filmly 播放流配置收窄 — 这套配置只对对应的客户端生效，兼容性记录同步补上。",
      ] },
    ],
  },
  {
    version: "0.1.17",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "✨ feat(web): 详情页信息更全 — Emby/Filmly 的详情、媒体流、人物头像和图片尺寸都补齐了。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(auth): token 写法和媒体策略 — 几种常见的 Emby token 写法都能认，媒体策略校验和媒体流类型判断的问题也修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor: Logo 换新 — 旧的 Logo 图已经替换成新的。",
      ] },
    ],
  },
  {
    version: "0.1.16",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 章节数据能存下来了 — 章节结果会持久化保存，片头片尾交给插件检测，也接上了 TheIntroDB 这个在线章节源，检测任务可以按媒体库调度。",
        "✨ feat(metadata): 章节配置和管理入口 — 章节来源能配，检测任务能管，Emby 那边会输出章节，管理台也加了对应入口。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 章节只处理剧集 — 只有剧集和分集才跑章节识别，不同来源的结果各自隔开；插件 SDK 和兼容性边界一起更新。",
      ] },
    ],
  },
  {
    version: "0.1.15",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "✨ feat(metadata): 本地 NFO 元数据更完整 — 补上了 NFO 的丰富元数据、演员关系快照、人物头像和条目统计。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(metadata): 快照和演员信息丢失 — NFO 快照失效、派生数据降级、演员信息丢失这几个问题修好了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(web): 详情页降级更顺 — NFO 和演员信息的投影合到一处，数据不全时详情页的降级处理也顺了。",
      ] },
    ],
  },
  {
    version: "0.1.14",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "✨ feat(image): 元数据对象和图片目录 — 加了元数据和图片的目录结构，NFO 里的演职员能读出来，插件包按机器架构装对应版本。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): 写回重试和外键迁移 — 元数据写回失败后的重试补齐了，STRM 探测任务的外键迁移问题也修了。",
      ] },
      { kind: "changed", items: [
        "🔧 build(plugin): 插件和构建调整 — TMDb 插件升级，插件打包、Docker 启动和商店来源设置跟着调了。",
      ] },
    ],
  },
  {
    version: "0.1.13",
    date: "2026-08-11",
    sections: [
      { kind: "added", items: [
        "✨ feat(playback): STRM 解析和缩略图 — 补了 STRM 的解析器与缩略图链路、能长期保存的用户头像、STRM media-info 插件，插件商店也支持远程来源。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 层级、首页和缩略图 — Emby/Filmly 的层级显示、首页布局、STRM 缩略图回退这几处都修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(probe): STRM 有变化就只探测它 — 实时增量扫描发现某个 STRM 来源变了，就给它单独建一个定向探测任务，不用整库重扫；首页加载也顺手调了。",
      ] },
    ],
  },
  {
    version: "0.1.12",
    date: "2026-08-11",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(probe): STRM 探测结果缺信息 — 探测出来的结果没有媒体信息，探测目标又被过滤得太早，两处一起修了。",
      ] },
    ],
  },
  {
    version: "0.1.11",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "✨ feat(probe): STRM 定时探测 — 可以配定时探测任务、管执行记录，实时元数据匹配也加了个开关。",
      ] },
    ],
  },
  {
    version: "0.1.10",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(deploy): Docker 镜像少文件 — 构建用的占位文件路径修好了，STRM media-info 插件也打进镜像里。",
      ] },
      { kind: "changed", items: [
        "🔧 build(build): 构建缓存调了一遍 — 各插件版本对齐，插件、Rust 依赖和 Web 资源的构建缓存都收拾过了。",
      ] },
    ],
  },
  {
    version: "0.1.9",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "✨ feat(media): 任务记录显示所属媒体库 — 任务列表里能直接看到这个任务属于哪个库。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(web): 排序偏好不生效 — 媒体库排序偏好和首页排序没被应用的问题修了。",
      ] },
      { kind: "changed", items: [
        "⚡ perf(api): 并发和分页加了边界 — 扫描、探测和后台任务有并发和队列上限，大库查询补了分页和批处理。",
      ] },
    ],
  },
  {
    version: "0.1.8",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(probe): 图片命名和探测释放 — 电影图片的命名、探测完后的资源释放、Emby 文件夹和最新内容的兼容问题都修了。",
      ] },
      { kind: "changed", items: [
        "⚡ perf(db): 查询改成有界批量 — 扫描、目录和 STRM 查询改成有界批量加翻页，并加了内存保护。",
      ] },
    ],
  },
  {
    version: "0.1.7",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(metadata): 候选失败分类等三处 — 元数据候选失败时的分类、实时 watcher 对文件变化的核对、横向滚动箭头，这几处都修了。",
      ] },
      { kind: "changed", items: [
        "♻️ refactor(metadata): 元数据后台处理 — 元数据的后台处理流程调整了，扫描性能的验证记录也补齐。",
      ] },
    ],
  },
  {
    version: "0.1.6",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix: 横向媒体栏箭头跟手了 — 箭头状态和实际滚动位置对得上，以前会不同步。",
      ] },
      { kind: "changed", items: [
        "⚡ perf(scan): 扫描性能记录补全 — 把后台扫描的性能验证结果记了下来。",
      ] },
    ],
  },
  {
    version: "0.1.5",
    date: "2026-08-09",
    sections: [
      { kind: "changed", items: [
        "♻️ refactor(auth): 只有版本号变动 — 没有额外的用户可见变化。",
      ] },
    ],
  },
  {
    version: "0.1.4",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(emby): Redia Emby 兼容回退 — 针对 Redia Emby 加了一条兼容回退。",
      ] },
    ],
  },
  {
    version: "0.1.3",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): 可选的 PostgreSQL 部署 — 多了一份可选的 PostgreSQL Compose 服务，migration 也支持这个后端。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): PostgreSQL 路径和几处页面问题 — PostgreSQL 这条路修通了，媒体库对话框、混合库图片、播放诊断和 Emby ID 过滤也跟着修好。",
      ] },
    ],
  },
  {
    version: "0.1.2",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "✨ feat(db): 首次引导选数据库 — 第一次启动能在 SQLite 和 PostgreSQL 之间选，跨后端存储和结构化日志归档也一起补上。",
      ] },
      { kind: "fixed", items: [
        "🐛 fix(db): PostgreSQL 下的几个问题 — 搜索和健康诊断、bootstrap、合集可见性、日志导出限制这几处都修了。",
      ] },
    ],
  },
  {
    version: "0.1.1",
    date: "2026-08-08",
    sections: [
      { kind: "fixed", items: [
        "🐛 fix(metadata): 元数据刷新失败不再连累别人 — 一次本地元数据刷新失败不会带崩其它流程；Emby 媒体库详情的兼容问题也顺手修了。",
      ] },
    ],
  },
  {
    version: "0.1.0",
    date: "2026-08-02",
    sections: [
      { kind: "added", items: [
        "✨ feat(core): Rust 模块化单体服务 — Lux 的服务在这一版立起来了，初始化认证、SQLite migration、统一错误处理和可观测性基础都有了。",
        "✨ feat(db): 媒体库和索引 — 电影、电视剧和混合媒体库，增量扫描、访问控制，以及 NFO/图片/ffprobe 索引和元数据写回。",
        "✨ feat(playback): 播放和个人数据 — 剧集层级、合集、字幕、本地直放、`.strm`、多版本媒体源、进度、收藏、搜索和下载权限。",
        "✨ feat(web): Lux Web 端和管理台 — 登录、浏览、搜索、详情、继续观看、用户权限、元数据审核和任务诊断都在。",
        "✨ feat(deploy): 部署与验证 — Docker/Compose 部署、ARM64 验证、浏览器 smoke、性能基线和恢复演练。",
      ] },
    ],
  },
];
