# 构建产物与临时文件位置规范

> 适用对象：在本机（macOS / arm64）开发 Lux 的人或自动化 agent  
> 规范状态：强制。与本文冲突的历史命令、脚本和临时记录以本文为准。

## 1. 为什么必须写进项目目录

这台开发机有两块盘：

| 磁盘 | 挂载点 | 容量 | 用途 |
| --- | --- | --- | --- |
| 内置 SSD | `/`、`/private/tmp`、`/var/folders`、`~/Library/Caches` | 228 GiB（长期只剩 20–30 GiB） | 系统盘，禁止承载项目构建负载 |
| 外置盘 | `/Volumes/Toshiba` | 1.7 TiB（长期剩余 1.5 TiB 以上） | 项目本体 `mywork/Lux`，空间充足 |

Lux 的 Rust 增量构建、`cargo test` 目标、SQLite fixture、Web 构建缓存和 Playwright 截图都是
GB 级写入。只要这些写入落到内置盘，就会同时触发三件事：

1. 内置盘可用空间被快速吃光，APFS 快照和交换分区失去回旋余地；
2. 系统在后台回收空间、重写快照时持续高 I/O，整机卡死、Finder 无响应；
3. 崩溃后残留的构建目录没人清理，下一次构建继续叠加，形成恶性循环。

2026-11 的实测：`/private/tmp` 下累积了约 24 GiB 的旧构建目录（`lux-*` 编译树、Swift
`lc04x-*` 派生数据），而外置盘上项目自己的 `target/` 已经有 164 GiB，说明构建本身跑在外置盘，
真正拖垮内置盘的是散落在 `/private/tmp`、`$TMPDIR`、各工具缓存目录里的临时产物。

**结论：项目的所有构建产物、缓存、临时数据、测试输出都必须写在项目文件夹内。**

## 2. 硬性规则

1. 构建产物只写 `$LUX_ROOT/target`（Rust）和 `$LUX_ROOT/web/dist`、`$LUX_ROOT/web/node_modules`（Web）。
2. 临时目录只写 `$LUX_ROOT/.tmp`，临时文件必须在同一次命令结束时清理。
3. 禁止把 `/tmp`、`/private/tmp`、`/var/tmp`、`/var/folders/...`、`~/Library/Caches`、`~/.cache`
   当作项目工作目录、构建输出目录、基准测试目录或截图目录。
4. 禁止把仓库、仓库副本、`target/`、`node_modules/` 拷到内置盘上编译或对比。
5. 禁止用 `mktemp -d` 默认的 `/tmp` 承载构建；新脚本统一用
   `mktemp -d "${LUX_ROOT}/.tmp/<用途>.XXXXXX"`（现有脚本的 `${TMPDIR:-/tmp}` 在按第 3 节
   导出 `TMPDIR` 后同样落在项目目录）。
6. 任何需要大空间的实验（A/B 构建、release 基准、PostgreSQL 数据目录、播放兼容性样本）都放在
   `$LUX_ROOT/.tmp` 或 `$LUX_ROOT/output` 下，结束即删。
7. 提交前确认 `git status` 干净：`.tmp/`、`target/`、`output/`、`dist/` 已在 `.gitignore` 中，
   规范文档本身不引入被跟踪的产物。

## 3. 标准环境变量

每次打开新 shell（或 CI 步骤）先加载项目自带的环境脚本：

```bash
cd /Volumes/Toshiba/mywork/Lux
source scripts/lux-env.sh
```

等价的显式写法：

```bash
export LUX_ROOT=/Volumes/Toshiba/mywork/Lux
export CARGO_TARGET_DIR="$LUX_ROOT/target"
export TMPDIR="$LUX_ROOT/.tmp"
export TMP="$TMPDIR"
export TEMP="$TMPDIR"
export SQLITE_TMPDIR="$TMPDIR"
export XDG_CACHE_HOME="$LUX_ROOT/.cache"
export PNPM_HOME="$LUX_ROOT/.pnpm"
export PLAYWRIGHT_BROWSERS_PATH="$LUX_ROOT/.playwright-browsers"
mkdir -p "$TMPDIR" "$XDG_CACHE_HOME" "$PNPM_HOME" "$PLAYWRIGHT_BROWSERS_PATH"
```

| 变量 | 作用 | 默认位置的后果 |
| --- | --- | --- |
| `CARGO_TARGET_DIR` | Rust 构建/测试产物 | 默认就是项目 `target/`，显式导出防止被 IDE 或别的 shell 改到别处 |
| `TMPDIR` | 所有遵守 POSIX 的工具的临时目录（cargo 中间文件、测试 fixture、`mktemp`、Chromium 用户目录） | macOS 默认在 `/var/folders/...`，即内置盘 |
| `SQLITE_TMPDIR` | SQLite 排序/日志临时文件 | 未设置时回退 `TMPDIR`，再回退 `/var/tmp` |
| `XDG_CACHE_HOME` | 遵循 XDG 的 CLI 缓存（如部分 Node/Python 工具） | `~/.cache`，内置盘 |
| `PLAYWRIGHT_BROWSERS_PATH` | Playwright 浏览器本体与下载缓存 | `~/Library/Caches/ms-playwright`，内置盘 |
| `PNPM_HOME` / 仓库内 `.pnpm-store` | pnpm 内容寻址存储 | 全局 `~/Library/pnpm`，内置盘 |

macOS 系统默认的 `TMPDIR` 带尾斜杠（`/var/folders/.../T/`）；本项目脚本按目录使用、不带尾斜杠，
两种写法等价，不要为了"保持系统格式"回退到内置盘路径。

### 3.1 TMPDIR 太长的特例

`/Volumes/Toshiba/mywork/Lux/.tmp` 已经 37 个字符，个别工具（Unix domain socket、`sun_path`
104 字节上限、超长 fixture 路径）可能报 `path too long`。此时不要退回 `/tmp`，而是建一个短
别名再指过去：

```bash
ln -sfn /Volumes/Toshiba/mywork/Lux/.tmp /Volumes/Toshiba/.lt
export TMPDIR=/Volumes/Toshiba/.lt
```

## 4. 目录布局

| 路径 | 内容 | 可删除 |
| --- | --- | --- |
| `target/` | Rust 构建、测试二进制、增量缓存 | 可，`cargo clean` 或 `rm -rf target` |
| `web/node_modules/`、`web/dist/` | Web 依赖与生产构建 | 可，重装/重建即可 |
| `.tmp/` | 临时 fixture、基准工作目录、截图、日志、数据库副本 | 可，`rm -rf .tmp/*` |
| `.cache/` | 工具缓存（XDG） | 可 |
| `.pnpm/` | pnpm 全局 home（`PNPM_HOME`） | 可 |
| `.pnpm-store/` | pnpm 内容存储 | 可，`pnpm store prune` |
| `.playwright-cli/`、`.vite/`、`.tscli/` | 本地工具输出 | 可 |
| `.playwright-browsers/` | Playwright 浏览器二进制 | 可，需要时重新下载 |
| `output/` | 手册、发布打包等本地产物 | 可 |

以上目录均已列入 `.gitignore`，不得提交。

## 5. 各工具的落盘位置

### 5.1 Rust / Cargo

```bash
source scripts/lux-env.sh
cargo build --locked
cargo test --locked --lib <module>
cargo clippy --locked --all-targets --all-features -- -D warnings
```

- 不设 `CARGO_TARGET_DIR` 时 cargo 默认写 `<workspace>/target`，仍在外置盘；但 agent 常在临时
  shell 里带 `CARGO_TARGET_DIR=/tmp/...` 做"并行 A/B"，这是被禁止的写法，A/B 目标目录应写成
  `$LUX_ROOT/.tmp/ab-<用途>`，结束删除。
- `cargo test` 的 fixture 通过 `std::env::temp_dir()` 取 `TMPDIR`，加载脚本后自动落在项目内。

### 5.2 pnpm / Vite / Vitest

```bash
pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
```

`node_modules`、Vite 缓存、`web/dist` 都在项目内；pnpm 全局存储通过 `PNPM_HOME` 与仓库内
`.pnpm-store` 固定，不要用 `pnpm store path` 指到内置盘。

### 5.3 Playwright

```bash
export PLAYWRIGHT_BROWSERS_PATH="$LUX_ROOT/.playwright-browsers"
pnpm --dir web exec playwright test
```

截图、trace、报告通过 `--output "$LUX_ROOT/.tmp/playwright"` 或配置项落在项目内。浏览器下载
（约 500 MiB）只在首次需要时发生，迁移旧缓存用
`mv ~/Library/Caches/ms-playwright "$LUX_ROOT/.playwright-browsers"`，然后删掉旧路径。

### 5.4 基准与压测

`docs/PERFORMANCE.md` 里的历史命令大量使用 `/tmp/...`。新记录一律改用
`$LUX_ROOT/.tmp/perf-<日期>-<主题>`；已经存在的历史记录保持原样，不追溯改写，但不得当作新基准
的模板照抄。仓库自带脚本（`scripts/run-performance.sh`、`scripts/probe-smoke.sh`、
`scripts/mount-loss-smoke.sh`、`scripts/disk-write-fault-smoke.sh` 等）走 `${TMPDIR:-/tmp}`，加载
`scripts/lux-env.sh` 后即符合规范；修改这些脚本时顺手把默认值换成 `${LUX_ROOT}/.tmp` 更好。

### 5.5 Docker

Docker 的构建缓存存放在 Docker Desktop 虚拟磁盘映像里（内置盘
`~/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw`），无法用 `TMPDIR` 改变：

- 需要大规模 `docker compose build` 时，先在 Docker Desktop 设置里把磁盘映像位置迁到
  `/Volumes/Toshiba`，或
- 构建前后执行 `docker builder prune -af` / `docker system prune -af`，把缓存峰值压在内置盘可承受
  的范围内，并确认 `df -h /` 未被吃满。

不要把 Docker build context、导出镜像的 tar 包写到 `/private/tmp`。

### 5.6 Xcode / Swift / clang 派生数据

同机其他工程（如 `luxkit-iossim`、`lc045-*`、`lc046`）的派生数据默认落在 `/private/tmp` 或
`~/Library/Developer/Xcode/DerivedData`，都是内置盘。给这类命令显式加
`xcodebuild -derivedDataPath "$LUX_ROOT/.tmp/derived-data"` 或
`swift build --scratch-path "$LUX_ROOT/.tmp/swift-scratch"`，或先 `source scripts/lux-env.sh`
让 `TMPDIR` 生效。这类目录是本次系统卡死的主要残留来源，用完必须删除。

## 6. 验证

执行任何构建前后，用下面几条命令确认没有写到内置盘：

```bash
# 1. 环境变量是否生效
source scripts/lux-env.sh
echo "TMPDIR=$TMPDIR"
python3 -c 'import tempfile; print(tempfile.gettempdir())'   # 必须显示项目 .tmp
echo "CARGO_TARGET_DIR=$CARGO_TARGET_DIR"

# 2. 两块盘的余量
df -h / /Volumes/Toshiba

# 3. 构建后内置盘临时目录不应增长（记录前后差值）
du -sh /private/tmp
du -sh "${TMPDIR}" 2>/dev/null

# 4. 找出新的内置盘残留
du -sh /private/tmp/* 2>/dev/null | sort -hr | head -10
```

预期：第 1 步输出的临时目录在 `/Volumes/Toshiba/...` 下；第 3 步 `/private/tmp` 在构建前后
基本不变；第 4 步没有出现本次任务新建的目录。

## 7. 清理与故障排查

内置盘吃紧时的安全清理顺序（先看内容再删，不要盲删系统目录）：

```bash
# 仅列出，不删除
du -sh /private/tmp/* 2>/dev/null | sort -hr | head -20

# 删除本项目已知的临时构建树（按实际名称确认后执行）
rm -rf /private/tmp/lux-* /private/tmp/lc04* 2>/dev/null

# 项目内清理
rm -rf .tmp/*                 # 临时 fixture / 基准 / 截图
cargo clean                   # Rust 产物（可重新构建，耗时较长）
pnpm --dir web store prune    # pnpm 未引用包
docker builder prune -af      # Docker 构建缓存
```

排查"到底谁在写内置盘"：

```bash
df -h /                                   # 先确认余量
sudo lsof +c0 +D /private/tmp 2>/dev/null | head   # 仍被占用的临时文件
fs_usage -w -f filesys | grep -E '/private/tmp|/var/folders'  # 实时写入监控（需 sudo）
```

`/private/tmp` 里存在 `lux-*`、`lc0*`、`*.build` 之类目录且时间戳是几天前的，通常是崩溃遗留，
可直接删除。

## 8. 与既有文档的关系

- `AGENTS.md`：把本文列为 agent 的强制规则，摘要见"Build output location"一节。
- `docs/LUX-DEVELOPMENT.md` 第 9.1 节：开发命令前置的环境约定。
- `docs/PERFORMANCE.md`：性能记录中的 `/tmp` 命令属于历史证据，新基准按本文执行。
- `scripts/lux-env.sh`：本文第 3 节环境变量的可执行版本，`source` 后即满足全部规则。
