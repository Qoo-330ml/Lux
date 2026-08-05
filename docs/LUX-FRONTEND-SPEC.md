# Lux Web 前端设计规范与 UI/UX 改造方案 (Lux Web Frontend Design System)

> **文档状态**：草案 / 待评审  
> **适用版本**：Lux Web v1.0+  
> **目标**：将 Lux Web 前端从传统的简单后台界面重构成具备**影院级沉浸感、现代高级美感与极致流畅交互**的现代媒体中心 UI。

---

## 1. 设计愿景与核心原则

Lux 是独立自主的个人媒体服务端。Web 前端不仅要满足管理员高效运维的需求，更要为普通用户提供媲美 Apple TV+、Netflix 和 Infuse 的顶级视觉与观影体验。

### 1.1 三大核心设计原则

1. **影院级沉浸感 (Cinematic Immersion)**
   - 采用黑曜石色调沉浸暗黑主题（Obsidian Dark Theme）。
   - 充分利用高质量海报与剧照，搭配动态高斯模糊背景与渐变遮罩（Ambient Blur Background），让视觉核心聚焦于媒体内容本身。
   - 引入现代化磨砂玻璃效应（Glassmorphic Blur），提升界面深邃感与层次感。

2. **现代化视觉语言 (Modern Aesthetic)**
   - 精细设计的颜色 Token 与高对比度文字层级，避免枯燥纯黑或低质灰色。
   - 紫蓝高光渐变（Indigo-to-Violet Accent）彰显极客与科技美感。
   - 全局圆角化（Border Radius）与精细微阴影（Micro Shadows）。

3. **极致流畅与高响应度 (Fluid Micro-Interactions)**
   - 骨架屏（Skeleton Screen）过渡，告别硬切与白屏。
   - 优雅的 Hover 浮起与悬停光效，极短响应延迟（150ms-250ms 动效）。
   - 搜索与筛选毫秒级响应，快捷键导航（全屏 / 搜索 / 播放）。

---

## 2. 基础设计 Token 规范 (Design Tokens)

### 2.1 色彩系统 (Color Palette)

Lux Web 全局统一采用基于 HSL/RGB 变量的深色主题设计体系：

| Token 名称 | 十六进制 / RGBA | 用途与视觉描述 |
| :--- | :--- | :--- |
| `--bg-base` | `#080A0F` | 主背景色：极深蓝黑黑曜石 |
| `--bg-surface` | `#111520` | 基础卡片/侧边栏背景 |
| `--bg-surface-elevated` | `rgba(255, 255, 255, 0.05)` | 浮动层/磨砂卡片（搭配 `backdrop-filter`） |
| `--bg-glass` | `rgba(17, 21, 32, 0.75)` | 顶部导航栏 / 浮动弹窗磨砂遮罩 |
| `--color-accent` | `#6366F1` | 品牌主色（Indigo 500） |
| `--color-accent-hover` | `#4F46E5` | 品牌主色 Hover 状态 |
| `--gradient-primary` | `linear-gradient(135deg, #6366F1 0%, #A855F7 100%)` | 渐变高光按钮与聚焦边框 |
| `--text-primary` | `#F8FAFC` | 一级文字：极高对比度白 (Slate 50) |
| `--text-secondary` | `#94A3B8` | 二级文字：次要描述 / 元数据 (Slate 400) |
| `--text-muted` | `#64748B` | 三级文字：占位符 / 禁用态 (Slate 500) |
| `--border-subtle` | `rgba(255, 255, 255, 0.08)` | 细微边框线，勾勒几何轮廓 |
| `--border-highlight` | `rgba(168, 85, 247, 0.3)` | 聚焦/高亮边框 |
| `--status-success` | `#10B981` | 状态：正常 / 已匹配 / 成功 |
| `--status-warning` | `#F59E0B` | 状态：待处理 (Pending) / 元数据匹配低置信度 |
| `--status-error` | `#EF4444` | 状态：失败 / 离线 / 扫描异常 |

---

### 2.2 排版与字体层级 (Typography)

* **推荐字体**：优先使用现代无衬线字体 `Inter`, `-apple-system`, `BlinkMacSystemFont`, `"Segoe UI"`, `Roboto`；代码与技术参数使用 `Fira Code`, `JetBrains Mono`。
* **字号与行高层级**：

| 层级 (Level) | 字号 (Font Size) | 行高 (Line Height) | 字重 (Font Weight) | 用途示例 |
| :--- | :--- | :--- | :--- | :--- |
| **Display Banner** | `40px / 2.5rem` | `1.2` | 800 (Bold) | 海报 Banner 大标题 |
| **Title H1** | `28px / 1.75rem` | `1.3` | 700 (Bold) | 媒体库名称 / 详情页标题 |
| **Title H2** | `20px / 1.25rem` | `1.4` | 600 (SemiBold)| 模块栏目标题 (如: 继续观看) |
| **Body Large** | `16px / 1rem` | `1.5` | 500 (Medium) | 卡片标题 / 关键属性 |
| **Body Regular** | `14px / 0.875rem`| `1.5` | 400 (Regular) | 剧情简介 / 状态描述 |
| **Caption / Badge**| `12px / 0.75rem` | `1.4` | 600 (SemiBold)| 4K / HDR / 评分 / 年份标签 |

---

### 2.3 间距、圆角与阴影 (Spacing, Radius & Shadows)

- **圆角规范**：
  - 小组件/标签 (`--radius-sm`): `6px`
  - 媒体海报卡片 (`--radius-md`): `12px`
  - 模态框/大容器 (`--radius-lg`): `20px`
  - 胶囊按钮 (`--radius-full`): `9999px`
- **阴影与光效**：
  - 普透层阴影: `box-shadow: 0 10px 30px -10px rgba(0, 0, 0, 0.5);`
  - 悬停霓虹光效 (Glow): `box-shadow: 0 0 25px rgba(99, 102, 241, 0.35);`

---

## 3. 核心视图与组件设计规范

### 3.1 顶部导航与全局搜索 (Header & Navigation)

```
+------------------------------------------------------------------------------------+
| 🍿 Lux  [电影库] [剧集库] [合集]   | 🔍 搜索电影、剧集、演职人员 (Cmd+K) |  (⚙️) (👤 Admin) |
+------------------------------------------------------------------------------------+
```
- **视觉特征**：吸顶半透明磨砂导航 (`backdrop-filter: blur(16px)` + `--bg-glass`)，向下滚动时产生细微描边。
- **全局搜索框**：按下 `Cmd+K` / `Ctrl+K` 调出浮动全局搜索 Modal，输入即时呈现包含海报图、类型与年份的带缩略图下拉列表。

---

### 3.2 首页 Hero Banner 巨幕区

```
+------------------------------------------------------------------------------------+
| [背景图: 动态高斯模糊 + 渐变渐隐遮罩]                                                 |
|                                                                                    |
|  沙丘2 (Dune: Part Two)                                                            |
|  [ 2024 ] [ 4K HDR ] [ Dolby Atmos ] [ ⭐️ 8.6 ] [ 动作 / 科幻 ]                       |
|  保罗·厄崔迪与契妮以及弗雷曼人联手，踏上了对毁灭他家族的阴谋者的复仇之路...                    |
|                                                                                    |
|  [ ▶️ 立即播放 ]   [ ℹ️ 详细信息 ]   [ ❤️ 收藏 ]                                     |
+------------------------------------------------------------------------------------+
```
- **渐变遮罩**：从左至right、从下至上施加双重黑色渐变（`linear-gradient(to right, #080A0F 30%, transparent 100%)`），确保文字完全清晰可读。
- **操作按钮**：主按钮为高亮紫蓝渐变，辅按钮为半透明玻璃底衬。

---

### 3.3 媒体海报卡片 (Media Poster Card Grid)

海报比例严格保持 **2:3**，采用自适应 Grid 布局（`grid-template-columns: repeat(auto-fill, minmax(180px, 1fr))`）。

```
+-------------------+   Hover 交互状态:
| [海报图片]         |   1. Card 轻微上浮 (`transform: translateY(-6px) scale(1.03)`)
|                   |   2. 呈现柔和紫光外阴影
| [ 4K ]   [ ⭐️ 8.5] |   3. 浮现半透明快捷播放悬浮按钮
|-------------------|   4. 显示未看 / 播放进度条底衬
| 标题名称           |
| 2024 • 电影        |
+-------------------+
```
- **继续观看卡片 (Continue Watching)**：
  - 使用 **16:9** 剧照/截图卡片。
  - 底部叠加带有紫粉渐变的播放进度条（Progress Bar），右下角标注剩余时长（例如 `"还剩 25 分钟"`）。

---

### 3.4 媒体详情页 (Media Details View)

- **顶部 Banner**：全宽高清 Backdrop 背景，搭配 60% 透明度渐变层。
- **信息分栏**：
  - 左侧：高质量海报浮雕展示 + 媒体版本下拉选择器（如: `4K Remux (45.2 GB)` / `1080p Web-DL (4.1 GB)`）。
  - 右侧：标题、别名、演职员标签、技术参数 Badge（HEVC, AV1, TrueHD 7.1, HDR10+）、剧情简介、音轨与外挂字幕列表。
- **剧集选择器 (Season / Episode Accordion)**：
  - 季选择 Tabs (第一季, 第二季...)。
  - 单集采用列表或小方阵网格，显示集数、单集标题、时长及播放进度。

---

### 3.5 Web 视频播放器 (Custom Native Video Player)

必须摆脱原生 HTML5 `<video>` 默认难看的控制条，使用极简沉浸式 UI 封装：

- **控制条自动隐藏**：无鼠标移动 3 秒自动隐去，指针离屏即刻隐藏。
- **自定义 Scrubber 进度条**：鼠标 Hover 进度条显示时间刻度悬停提示。
- **音轨与字幕切换**：弹出式优雅 Modal，清晰标注语言与格式（如: `中文 (外挂 ASS)` / `English (内嵌 SRT)`）。
- **键盘快捷键支持**：
  - `Space` / `K`: 播放/暂停
  - `F`: 全屏切换
  - `M`: 静音
  - `Left` / `Right`: 快退 / 快进 5 秒
  - `Up` / `Down`: 音量调节

---

### 3.6 管理员控制台 (Admin Dashboard)

针对任务与元数据管理，设计数据密集型且条理清晰的监控界面：

1. **概览指标面板 (Metrics Stat Cards)**：
   - 媒体库条目总计、扫描队列状态（“正在扫描 / 空闲”）、磁盘/内存占用率、待匹配数量。
2. **待匹配 / 匹配低置信度队列 (Pending Queue)**：
   - 表格形式呈现原始文件名、解析建议标题、TMDb 置信度分值。
   - 提供“一键重新搜索匹配”弹窗与差异对比展示。
3. **元数据回写与监控卡片 (NFO Writeback & Tasks)**：
   - 实时日志流终端面板（带高亮 Console 样式）。
   - 可收起与可重试的任务状态列表。

---

## 4. 微动效与交互体验规范

1. **过渡时间 (Transitions)**：
   - 基础 Hover: `150ms cubic-bezier(0.4, 0, 0.2, 1)`
   - 弹窗/侧滑抽屉: `250ms cubic-bezier(0.16, 1, 0.3, 1)`
2. **骨架屏加载 (Skeleton Loading)**：
   - 媒体海报在加载中时，显示高光扫过微动效（Shimmer Animation）：
     `background: linear-gradient(90deg, #131722 25%, #1D2433 50%, #131722 75%);`
3. **响应式断点 (Breakpoints)**：
   - 移动端 (`< 640px`): 侧边栏转为底部导航栏，海报网格 2 列。
   - 平板端 (`640px - 1024px`): 海报网格 3-4 列。
   - 桌面端 (`> 1024px`): 海报网格 5-7 列，Hero Banner 完备展现。

---

## 5. CSS 架构与重构实施 Roadmap

### 5.1 CSS 变量结构 (`web/src/styles.css`)

推荐在 CSS 中定义清晰的规范变量：

```css
:root {
  /* Colors */
  --bg-base: #080a0f;
  --bg-surface: #111520;
  --bg-surface-elevated: rgba(255, 255, 255, 0.05);
  --bg-glass: rgba(17, 21, 32, 0.75);
  
  --color-accent: #6366f1;
  --color-accent-hover: #4f46e5;
  --gradient-accent: linear-gradient(135deg, #6366f1 0%, #a855f7 100%);
  
  --text-primary: #f8fafc;
  --text-secondary: #94a3b8;
  --text-muted: #64748b;
  
  --border-subtle: rgba(255, 255, 255, 0.08);
  --border-highlight: rgba(168, 85, 247, 0.35);

  /* Radius */
  --radius-sm: 6px;
  --radius-md: 12px;
  --radius-lg: 20px;
  --radius-full: 9999px;

  /* Transitions */
  --transition-fast: 150ms cubic-bezier(0.4, 0, 0.2, 1);
  --transition-normal: 250ms cubic-bezier(0.16, 1, 0.3, 1);
}
```

### 5.2 分步实施计划 (Implementation Steps)

- **Step 1**: 在 `web/src/styles.css` 中注入完整的 Design System 变量体系与基础重置样式（Reset & Utility Classes）。
- **Step 2**: 改造顶部 AppHeader 与全局导航组件，引入半透明磨砂与快捷搜索模态框。
- **Step 3**: 升级首页大屏，加入 Hero Banner 电影展示与滑动“继续观看”卡片。
- **Step 4**: 优化海报 Grid 卡片样式，添加 Hover 缩放、霓虹边框与磨砂标签。
- **Step 5**: 优化 Web 播放器控制栏与管理控制台监控卡片，提升全面美观度。
