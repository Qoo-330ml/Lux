# Lux Web 播放器第三方声明

本文档记录 LuxPlayer 直接复制、改造或作为实现参考的第三方代码。只引用公开设计或浏览器标准，不计入代码衍生台账；如果 Lux 将来复制代码，即使只复制一个文件中的一部分，也必须在本文件增加记录。

## ArtPlayer

- 项目：ArtPlayer.js
- 上游仓库：https://github.com/zhw2590582/ArtPlayer
- 官方站点：https://artplayer.org
- 许可证：MIT License
- 版权：Copyright (c) 2018 Harvey Zhao
- 参考快照：仓库 `master`，commit `02d1ded7b8601b8cc654e33d066d996968c7bdc0`，2026-08-28 核验
- 许可证原文：https://raw.githubusercontent.com/zhw2590582/ArtPlayer/02d1ded7b8601b8cc654e33d066d996968c7bdc0/LICENSE
- 当前 Lux 使用状态：LUX-207 已按台账中列出的三个模块借鉴交互与边界处理思路；Lux 未复制 ArtPlayer
  源码，也未将 `artplayer` 加入依赖。LUX-208 未新增 ArtPlayer 衍生代码；LUX-209 只核验官网演示页的可见
  布局与控件状态，不复制 ArtPlayer DOM、CSS、图标、品牌或演示资产。LUX-212 至 LUX-214 继续使用
  Lux 自有字幕/弹幕解析、Worker 协议、轨道调度、DOM 和生命周期；ArtPlayer 的字幕生命周期、弹幕 lane
  调度和控制层只作为公开行为参考，没有新增复制或改造代码。LUX-215 重新核验上述固定快照，只验证
  生命周期、lane、安全边界和性能问题，没有新增复制、改造或运行时依赖。LUX-217 的画面比例、镜像
  与设置组件继续使用 Lux 自有类型、React DOM 和呈现映射，ArtPlayer 只作为选项集合与生命周期边界参考。
  LUX-218 的字幕偏移继续使用 Lux 自有时间范围映射、原生 TextTrack cue 控制器、文本覆盖层和设置控件，
  ArtPlayer 只作为不可累计偏移、时长裁剪和销毁恢复边界参考。

MIT License 要求在软件的所有副本或实质性部分中保留版权和许可声明。Lux 对 ArtPlayer 的复制或改造必须保留上述信息，并在衍生文件中注明 Lux 修改边界。

## 衍生代码来源台账

后续每个复制或改造模块使用一行记录：

| 上游快照 | 上游路径 | Lux 路径 | 处理方式 | 变更摘要 | 许可证/notice |
|---|---|---|---|---|---|
| 待填 | 待填 | 待填 | 复制 / 改造 / 仅参考 | 待填 | MIT / 本文件 |
| `02d1ded7b8601b8cc654e33d066d996968c7bdc0` | `packages/artplayer/src/events/gestureInit.js`、`packages/artplayer/src/events/clickInit.js`、`packages/artplayer/src/control/progress.js` | `web/src/features/player/components/player-gestures.ts`、`web/src/features/player/PlayerPage.tsx`、`web/src/features/player/components/player-video-surface.tsx` | 仅参考 | 参考手势方向、时间轴局部坐标与单/双击冲突处理的问题边界；Lux 使用自己的 Pointer Events、pointer capture、状态回调、阈值和测试，未复制上游代码、DOM、CSS、图标或 API。 | MIT / 本文件 |
| `02d1ded7b8601b8cc654e33d066d996968c7bdc0` | `packages/artplayer/src/player/aspectRatioMix.js`、`packages/artplayer/src/player/flipMix.js`、`packages/artplayer/src/setting/aspectRatio.js`、`packages/artplayer/src/setting/flip.js` | `web/src/features/player/components/player-presentation.ts`、`web/src/features/player/components/player-settings-panel.tsx`、`web/src/features/player/components/player-video-surface.tsx` | 仅参考 | 参考 `default/4:3/16:9`、`normal/horizontal/vertical` 的稳定选项集合、容器内比例适配意图和镜像语义；Lux 使用自己的 TypeScript 联合类型、React 可访问控件和 CSSProperties 映射，未复制上游控制流、DOM、样式或事件 API。 | MIT / 本文件 |
| `02d1ded7b8601b8cc654e33d066d996968c7bdc0` | `packages/artplayer/src/player/eventInit.js` 的 `video:ended` 循环分支 | `web/src/features/player/PlayerPage.tsx` | 仅参考 | 参考循环开启时回到开头并继续播放、关闭时保留正常结束态的行为边界；Lux 读取当前 video 的标准 `loop` 属性，通过 Lux Runtime 执行 seek/play，并保持自有会话释放与错误分类，不复制上游事件、控件或状态结构。 | MIT / 本文件 |
| `02d1ded7b8601b8cc654e33d066d996968c7bdc0` | ArtPlayer 官方演示页默认控制层与弹幕插件演示页（2026-08-27 可见布局核验） | `web/src/features/player/components/player-controls.tsx`、`web/src/react.css` | 仅参考 | 核验播放、音量、时间、版本、截图、设置、画中画与全屏控件的可见顺序、46px 节奏、底部渐变层和时间轴密度；Lux 使用自身 React DOM、Lucide 图标、媒体会话和会话接口，未复制上游代码、DOM、CSS、图标或资源。 | MIT / 本文件 |
| `02d1ded7b8601b8cc654e33d066d996968c7bdc0` | `packages/artplayer/src/subtitle.js`、`packages/artplayer-plugin-danmuku/src/danmuku.js`、`packages/artplayer-plugin-danmuku/src/worker.js`、`packages/artplayer-plugin-danmuku/src/bilibili.js` | `web/src/features/player/caption-parser.ts`、`web/src/features/player/caption-parser-worker.ts`、`web/src/features/player/components/player-caption-overlay.tsx`、`web/src/features/player/danmaku.ts`、`web/src/features/player/danmaku-worker.ts`、`web/src/features/player/components/player-danmaku-overlay.tsx` | 仅参考 | 参考字幕切换/销毁、弹幕状态池、lane 防重叠、Worker 计算和 Bilibili XML 模式映射的问题边界；Lux 使用有界安全解析、request generation、同源 Lux API、React 文本节点和自有调度算法，未复制上游实现、DOM、CSS、图标、网络调用、发送器或热力图。 | MIT / 本文件 |
| `02d1ded7b8601b8cc654e33d066d996968c7bdc0` | `packages/artplayer/src/setting/subtitleOffset.js`、`packages/artplayer/src/player/subtitleOffsetMix.js` | `web/src/features/player/caption-offset.ts`、`web/src/features/player/components/player-settings-panel.tsx`、`web/src/features/player/components/player-caption-overlay.tsx`、`web/src/features/player/components/player-video-surface.tsx` | 仅参考 | 参考 `-10s～+10s`、0.1s 步进、从原始 cue 时间重新计算、按媒体时长裁剪和 track 生命周期恢复；Lux 使用自己的 TypeScript 时间范围函数、原生 TextTrack 控制器、React range 控件和覆盖层，未复制上游代码、DOM、样式或事件 API。 | MIT / 本文件 |

“仅参考”表示没有复制可受版权保护的代码，可以不放入 `vendor-derived` 目录；但仍应在设计文档中说明参考目的。复制或改造的代码必须经过 Lux 自己的类型、状态和安全边界，不得把上游插件的网络请求、远程资源、品牌资产或默认配置带入 Lux。

## 其他依赖

Web 播放已有依赖的许可证继续由包管理文件和发布构建记录负责；新增播放器依赖或 WASM 组件时，必须在引入同一提交中补充名称、版本、许可证、上游地址和发布包是否包含该组件。
