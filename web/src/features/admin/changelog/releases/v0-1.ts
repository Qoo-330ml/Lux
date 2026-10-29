import type { ChangelogRelease } from "../types";

export const changelogReleases01: ChangelogRelease[] = [
  {
    version: "0.1.18",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "通过 Emby 输出本地 NFO 丰富元数据，并增强 Filmly 媒体流和图片字段。",
      ] },
      { kind: "fixed", items: [
        "修复 Filmly 分集、next-up、season DTO、媒体流和主图回退问题。",
      ] },
      { kind: "changed", items: [
        "将 Filmly 播放流配置限制在对应客户端，并补充兼容性记录。",
      ] },
    ],
  },
  {
    version: "0.1.17",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "增强 Emby/Filmly 详情、媒体流、人物头像和图片尺寸信息。",
      ] },
      { kind: "fixed", items: [
        "兼容多种 Emby token 写法，修复媒体策略校验和媒体流类型问题。",
      ] },
      { kind: "changed", items: [
        "扩展目录详情与恢复播放回归覆盖，并更新 Logo 资源。",
      ] },
    ],
  },
  {
    version: "0.1.16",
    date: "2026-08-13",
    sections: [
      { kind: "added", items: [
        "增加章节持久化、片头片尾检测插件、TheIntroDB 在线章节源和按媒体库调度。",
        "增加章节来源配置、检测任务管理、Emby 章节输出及管理台支持。",
      ] },
      { kind: "changed", items: [
        "章节仅处理剧集/分集，结果按来源隔离，并更新插件 SDK 和兼容性边界。",
      ] },
    ],
  },
  {
    version: "0.1.15",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "增加本地 NFO 丰富元数据、演员关系快照、人物头像和条目统计。",
      ] },
      { kind: "fixed", items: [
        "修复 NFO 快照失效、派生数据降级和演员信息丢失问题。",
      ] },
      { kind: "changed", items: [
        "合并 NFO 与演员信息投影，并优化详情页降级处理。",
      ] },
    ],
  },
  {
    version: "0.1.14",
    date: "2026-08-12",
    sections: [
      { kind: "added", items: [
        "增加元数据对象/图片目录、NFO 演职员读取和按架构安装插件包。",
      ] },
      { kind: "fixed", items: [
        "修复元数据写回重试和 STRM 探测任务外键迁移问题。",
      ] },
      { kind: "changed", items: [
        "升级 TMDb 插件并调整插件打包、Docker 启动和商店来源设置。",
      ] },
    ],
  },
  {
    version: "0.1.13",
    date: "2026-08-11",
    sections: [
      { kind: "added", items: [
        "增加 STRM 解析器/缩略图链路、持久化用户头像、STRM media-info 插件和远程插件商店。",
      ] },
      { kind: "fixed", items: [
        "修复 Emby/Filmly 层级、首页布局和 STRM 缩略图回退问题。",
      ] },
      { kind: "changed", items: [
        "实时增量扫描为变化的 STRM 来源创建定向探测任务，并优化首页加载。",
      ] },
    ],
  },
  {
    version: "0.1.12",
    date: "2026-08-11",
    sections: [
      { kind: "fixed", items: [
        "修复 STRM 探测缺少媒体信息及目标被过早过滤的问题。",
      ] },
    ],
  },
  {
    version: "0.1.11",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "增加 STRM 定时探测任务配置、执行管理和实时元数据匹配开关。",
      ] },
    ],
  },
  {
    version: "0.1.10",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "修复 Docker 构建占位文件路径，并将 STRM media-info 插件纳入镜像。",
      ] },
      { kind: "changed", items: [
        "对齐插件版本，优化插件、Rust 依赖和 Web 资源的构建缓存。",
      ] },
    ],
  },
  {
    version: "0.1.9",
    date: "2026-08-10",
    sections: [
      { kind: "added", items: [
        "任务记录显示所属媒体库。",
      ] },
      { kind: "fixed", items: [
        "修复媒体库排序偏好和首页排序应用问题。",
      ] },
      { kind: "changed", items: [
        "为扫描、探测和后台任务增加并发/队列边界，并为大库查询补充分页和批处理。",
      ] },
    ],
  },
  {
    version: "0.1.8",
    date: "2026-08-10",
    sections: [
      { kind: "fixed", items: [
        "修复电影图片命名、媒体探测释放和 Emby 文件夹/最新内容兼容问题。",
      ] },
      { kind: "changed", items: [
        "扫描、目录和 STRM 查询改为有界批量、分页和内存保护。",
      ] },
    ],
  },
  {
    version: "0.1.7",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "修复元数据候选失败分类、实时 watcher 调和和横向滚动箭头问题。",
      ] },
      { kind: "changed", items: [
        "调整元数据后台处理，并完成扫描性能验证记录。",
      ] },
    ],
  },
  {
    version: "0.1.6",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "修复横向媒体栏箭头与实际滚动位置不同步。",
      ] },
      { kind: "changed", items: [
        "补充后台扫描性能验证记录。",
      ] },
    ],
  },
  {
    version: "0.1.5",
    date: "2026-08-09",
    sections: [
      { kind: "changed", items: [
        "完成版本号维护更新；该版本没有额外的独立用户可见变化。",
      ] },
    ],
  },
  {
    version: "0.1.4",
    date: "2026-08-09",
    sections: [
      { kind: "fixed", items: [
        "增加 Redia Emby 兼容回退。",
      ] },
    ],
  },
  {
    version: "0.1.3",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "增加可选 PostgreSQL Compose 服务和 migration 构建支持。",
      ] },
      { kind: "fixed", items: [
        "修复 PostgreSQL 路径，以及媒体库对话框、混合库图片、播放诊断和 Emby ID 过滤问题。",
      ] },
    ],
  },
  {
    version: "0.1.2",
    date: "2026-08-09",
    sections: [
      { kind: "added", items: [
        "支持首次引导选择 SQLite/PostgreSQL，增加跨后端存储和结构化日志归档。",
      ] },
      { kind: "fixed", items: [
        "修复 PostgreSQL 搜索/健康诊断、bootstrap、合集可见性和日志导出限制问题。",
      ] },
    ],
  },
  {
    version: "0.1.1",
    date: "2026-08-08",
    sections: [
      { kind: "fixed", items: [
        "隔离本地元数据刷新失败，并修复 Emby 媒体库详情兼容问题。",
      ] },
    ],
  },
  {
    version: "0.1.0",
    date: "2026-08-02",
    sections: [
      { kind: "added", items: [
    "建立 Lux Rust 模块化单体服务，涵盖初始化认证、SQLite migration、统一错误和可观测性基础。",
        "支持电影、电视剧和混合媒体库、增量扫描、访问控制、NFO/图片/ffprobe 索引及元数据写回。",
        "支持剧集层级、合集、字幕、本地直放、`.strm`、多版本媒体源、进度、收藏、搜索和下载权限。",
        "建立 Lux Web 与管理台，覆盖登录、浏览、搜索、详情、继续观看、用户权限、元数据审核和任务诊断。",
        "增加 Docker/Compose 部署、ARM64 验证、浏览器 smoke、性能基线和恢复演练。",
      ] },
    ],
  },
];
