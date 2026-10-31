import type { ChangelogRelease } from "../types";

export const changelogReleases02: ChangelogRelease[] = [
  {
    version: "0.2.9",
    date: "2026-08-27",
    sections: [
      { kind: "added", items: [
        "增加 Web 本地媒体播放会话、签名 Direct、0～4 档播放计划、服务端 HLS 和播放控制。",
        "增加 Redia 媒体源兼容、元数据补全资源管线和 Emby 迁移跳过报告。",
      ] },
      { kind: "fixed", items: [
        "修复 HLS 生命周期、`.strm` 越界进入转码、Redia 媒体源 ID、播放跳转和元数据重复请求问题。",
      ] },
      { kind: "changed", items: [
        "播放、HLS、`.strm` 和元数据处理均采用短期签名、有界资源配额和明确的远程访问边界。",
      ] },
    ],
  },
  {
    version: "0.2.8",
    date: "2026-08-25",
    sections: [
      { kind: "added", items: [
        "增加弹幕匹配、STRM 解析、统一任务历史、人物索引、元数据任务调度和持久化扫描变更集。",
        "增加 Emby 迁移工作区、用户状态/人物收藏导入、DisplayPreferences、人物搜索和有序刮削器配置。",
      ] },
      { kind: "fixed", items: [
        "修复 STRM 目标分类、任务管理、人物索引、插件更新、迁移恢复、多刮削器优先级和扫描旁车处理问题。",
      ] },
      { kind: "changed", items: [
        "统一计划与一次性任务记录，收紧插件更新、迁移密钥和资源配额；扫描拆分为发现、调和与后处理阶段。",
      ] },
    ],
  },
  {
    version: "0.2.7",
    date: "2026-08-20",
    sections: [
      { kind: "added", items: [
        "增加 Matroska/HEVC 客户端播放回退、完整播放器控制栏、人物查询与详情、待确认批量操作和插件更新。",
        "增加扫描活动、规范人物身份、人物匹配审核和按用户保存的媒体库排序。",
      ] },
      { kind: "fixed", items: [
        "修复 Matroska 播放、扫描 checkpoint、人物 API/图片/NFO、元数据状态和插件更新问题。",
      ] },
      { kind: "changed", items: [
        "完成媒体能力探针阶段验收，优化快照缓存、扫描生命周期、人物资源目录和跨 provider 身份匹配。",
      ] },
    ],
  },
  {
    version: "0.2.6",
    date: "2026-08-17",
    sections: [
      { kind: "added", items: [
        "增加 Emby 虚拟文件夹、共享 API Key、持久化 Webhook/outbox、通知管理、媒体能力探针和 HEVC fallback。",
        "增加 Emby 人物列表及其分页、权限和 API Key 兼容。",
      ] },
      { kind: "fixed", items: [
        "修复 Webhook 重试和安全校验、人物身份回填、混合媒体库目录及 Emby 请求解析问题。",
      ] },
      { kind: "changed", items: [
        "完善虚拟文件夹映射、人物索引、反向代理元数据、Provider ID 匹配、4K fallback 提示和 Docker 发布标签。",
      ] },
    ],
  },
  {
    version: "0.2.5",
    date: "2026-08-16",
    sections: [
      { kind: "added", items: [
        "增加服务器级共享管理员 API Key、混合媒体库类型和 TMDb 人物资料。",
      ] },
      { kind: "fixed", items: [
        "修复任务排序、旧版 STRM 计划、转发客户端 IP 和无音频流的 Emby 字段问题。",
      ] },
      { kind: "changed", items: [
        "将 API Key 改为受限文件存储，支持轮换、撤销和集中管理。",
      ] },
    ],
  },
  {
    version: "0.2.4",
    date: "2026-08-15",
    sections: [
      { kind: "fixed", items: [
        "修复 PostgreSQL 布尔值存储兼容性问题。",
      ] },
    ],
  },
  {
    version: "0.2.3",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "增加管理员删除用户功能。",
      ] },
      { kind: "fixed", items: [
        "修复后台扫描进度/取消响应和 PostgreSQL migration 校验和问题。",
      ] },
      { kind: "changed", items: [
        "优化扫描任务同步、取消流程及用户管理回归覆盖。",
      ] },
    ],
  },
  {
    version: "0.2.2",
    date: "2026-08-15",
    sections: [
      { kind: "added", items: [
        "支持媒体库多根路径和登录 session 恢复。",
      ] },
      { kind: "fixed", items: [
        "修复元数据任务误处理文件夹、媒体身份/路径不稳定和 migration 校验和问题。",
      ] },
      { kind: "changed", items: [
        "默认开启实时元数据匹配，增量扫描可自动补全缺失元数据。",
      ] },
    ],
  },
  {
    version: "0.2.1",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "增加管理台更新日志、元数据确认流程和共享表单控件。",
      ] },
      { kind: "fixed", items: [
        "修复登录 session、扫描并发和 Emby 条目 ID 兼容问题。",
      ] },
      { kind: "changed", items: [
        "优化首页性能，并将元数据确认统一纳入管理流程。",
      ] },
    ],
  },
  {
    version: "0.2.0",
    date: "2026-08-14",
    sections: [
      { kind: "added", items: [
        "支持每个用户独立调整自动标记已看的播放阈值，并完善收藏、进度和继续观看链路。",
        "增加本地 NFO 丰富元数据、人物资源复用、插件卸载、主题资源和响应式首页。",
      ] },
      { kind: "fixed", items: [
        "修复 Emby/Filmly 和本地 NFO 演员身份、媒体流及图片回退问题。",
      ] },
      { kind: "changed", items: [
        "将自动已看阈值改为用户设置，并同步升级 SQLite/PostgreSQL 播放设置结构。",
      ] },
    ],
  },
];
