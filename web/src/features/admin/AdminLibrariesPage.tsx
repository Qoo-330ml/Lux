import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Copy,
  Database,
  HardDrive,
  Folder,
  Image,
  Languages,
  ListPlus,
  MinusCircle,
  MoreHorizontal,
  Pencil,
  Plus,
  RefreshCw,
  Save,
  Settings2,
  Sparkles,
  Upload,
  X,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { ChangeEvent, DragEvent, FormEvent, useEffect, useRef, useState } from "react";
import { LuxSelect } from "../../components/LuxSelect";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminLibrary, AdminLibraryScraper, AdminPlugin, ChapterSource, MediaStrategySettings, MetadataRefreshMode } from "../../lib/api/types";
import { orderLibraries } from "../account/account-settings";
import { DirectoryPicker } from "./DirectoryPicker";

export function AdminLibrariesPage() {
  const queryClient = useQueryClient();
  const libraries = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const libraryOrder = useQuery({ queryKey: queryKeys.libraryOrder, queryFn: () => api.libraryOrder() });
  const plugins = useQuery({ queryKey: queryKeys.adminPlugins, queryFn: () => api.adminPlugins() });
  const chapterSources = useQuery({ queryKey: queryKeys.adminChapterSources, queryFn: () => api.adminChapterSources() });
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [activeView, setActiveView] = useState<"libraries" | "advanced">("libraries");
  const [strategy, setStrategy] = useState<MediaStrategySettings | null>(null);
  const [strategySaved, setStrategySaved] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [name, setName] = useState("");
  const [kind, setKind] = useState("MOVIE");
  const [scrapers, setScrapers] = useState<AdminLibraryScraper[]>([]);
  const [chapterSourceId, setChapterSourceId] = useState("");
  const [createRealtimeWatchEnabled, setCreateRealtimeWatchEnabled] = useState(true);
  const [createRealtimeMetadataAutoMatch, setCreateRealtimeMetadataAutoMatch] = useState(true);
  const [createRootPath, setCreateRootPath] = useState("");
  const [createRootPaths, setCreateRootPaths] = useState<string[]>([]);
  const [createDirectoryPickerOpen, setCreateDirectoryPickerOpen] = useState(false);
  const [formError, setFormError] = useState("");
  const [createNotice, setCreateNotice] = useState("");
  const saveStrategy = useMutation({
    mutationFn: (next: MediaStrategySettings) => api.updateAdminSettings({ mediaStrategy: next }),
    onSuccess: (data) => {
      setStrategy(data.mediaStrategy);
      setStrategySaved(true);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminSettings });
    },
    onError: () => setStrategySaved(false),
  });
  useEffect(() => {
    if (settings.data?.mediaStrategy && strategy === null) setStrategy(settings.data.mediaStrategy);
  }, [settings.data?.mediaStrategy, strategy]);
  const itemsForScan = libraries.data?.libraries ?? [];
  const scanAll = useMutation({
    mutationFn: async () => {
      for (const library of itemsForScan) await api.startAdminScan(library.id);
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminHealth });
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() });
    },
  });
  const refreshAll = useMutation({
    mutationFn: async (mode: MetadataRefreshMode) => {
      for (const library of itemsForScan) await api.startLibraryMetadataRefresh(library.id, mode);
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() });
    },
  });
  function addCreateRootPath(path: string) {
    const normalizedPath = path.trim();
    if (!normalizedPath) return;
    if (createRootPaths.includes(normalizedPath)) {
      setFormError("该文件夹已经添加");
      return;
    }
    setCreateRootPaths((current) => [...current, normalizedPath]);
    setCreateRootPath("");
    setFormError("");
    setCreateDirectoryPickerOpen(false);
  }
  function removeCreateRootPath(path: string) {
    setCreateRootPaths((current) => current.filter((item) => item !== path));
  }
  const create = useMutation({
      mutationFn: async (values: { name: string; kind: string; scrapers: AdminLibraryScraper[]; chapterSourceId: string | null; rootPaths: string[]; realtimeWatchEnabled: boolean; realtimeMetadataAutoMatchEnabled: boolean }) => {
      const scraperInput = values.scrapers.length > 0
        ? { scrapers: values.scrapers.map(({ scraperId, role }) => ({ scraperId, role })) }
        : { scraperId: null };
      const response = await api.createAdminLibrary({
        name: values.name,
        kind: values.kind,
        ...scraperInput,
        ...(values.chapterSourceId ? { chapterSourceId: values.chapterSourceId } : {}),
        realtimeWatchEnabled: values.realtimeWatchEnabled,
        realtimeMetadataAutoMatchEnabled: values.realtimeMetadataAutoMatchEnabled,
      });
      const rootErrors: string[] = [];
      for (const rootPath of values.rootPaths) {
        try {
          await api.addAdminLibraryRoot(response.library.id, rootPath);
        } catch (error) {
          rootErrors.push(`${rootPath}：${error instanceof Error ? error.message : "文件夹添加失败"}`);
        }
      }
      return { ...response, rootErrors };
    },
    onSuccess: ({ rootErrors }) => {
      setName("");
      setScrapers([]);
      setChapterSourceId("");
      setCreateRealtimeWatchEnabled(true);
      setCreateRealtimeMetadataAutoMatch(true);
      setCreateRootPath("");
      setCreateRootPaths([]);
      setCreateDirectoryPickerOpen(false);
      setFormError("");
      setCreateNotice(rootErrors.length > 0 ? `媒体库已创建，但以下文件夹添加未完成：${rootErrors.join("；")}。请在“编辑”中检查并添加文件夹。` : "");
      setCreateOpen(false);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
    onError: (error) => setFormError(error.message),
  });

  function openCreate() {
    setName("");
    setKind("MOVIE");
    setScrapers([]);
    setChapterSourceId("");
    setCreateRealtimeWatchEnabled(true);
    setCreateRealtimeMetadataAutoMatch(true);
    setCreateRootPath("");
    setCreateRootPaths([]);
    setCreateDirectoryPickerOpen(false);
    setFormError("");
    setCreateNotice("");
    setCreateOpen(true);
  }

  if ((libraries.error && !libraries.data) || (libraryOrder.error && !libraryOrder.data)) {
    return <AdminLibraryState label={libraries.error?.message || libraryOrder.error?.message || "媒体库数据加载失败"} error />;
  }
  if (libraries.isPending || libraryOrder.isPending) {
    const label = libraries.isPending
      ? libraryOrder.isPending ? "正在读取媒体库及已保存的媒体库顺序…" : "正在读取媒体库…"
      : "正在读取已保存的媒体库顺序…";
    return <AdminLibraryState label={label} />;
  }

  const items = orderLibraries(libraries.data.libraries ?? [], libraryOrder.data?.libraryOrder ?? []);
  const pluginItems = plugins.data?.plugins ?? [];
  const chapterSourceItems = chapterSources.data?.sources ?? [];
  const auxiliaryLoading = plugins.isPending || chapterSources.isPending || settings.isPending;
  const auxiliaryError = plugins.error || chapterSources.error || settings.error;
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) {
      setFormError("请输入媒体库名称");
      return;
    }
    const pendingRootPath = createRootPath.trim();
    const rootPaths = pendingRootPath && !createRootPaths.includes(pendingRootPath)
      ? [...createRootPaths, pendingRootPath]
      : createRootPaths;
    create.mutate({
      name: name.trim(),
      kind,
      scrapers,
      chapterSourceId: supportsChapterSource(kind) ? chapterSourceId || null : null,
      rootPaths,
      realtimeWatchEnabled: createRealtimeWatchEnabled,
      realtimeMetadataAutoMatchEnabled: createRealtimeMetadataAutoMatch,
    });
  }

  return (
    <div className="lux-admin-page lux-admin-library-page">
      <header className="lux-library-management-header">
        <h1>媒体库</h1>
        <div className="lux-library-tabs" role="tablist" aria-label="媒体库视图">
          <button className={activeView === "libraries" ? "is-active" : ""} type="button" role="tab" aria-selected={activeView === "libraries"} onClick={() => setActiveView("libraries")}>媒体库</button>
          <button className={activeView === "advanced" ? "is-active" : ""} type="button" role="tab" aria-selected={activeView === "advanced"} onClick={() => setActiveView("advanced")}>高级</button>
        </div>
      </header>

      {activeView === "libraries" ? (
        <>
          {auxiliaryLoading ? <p className="lux-admin-muted" role="status">媒体库已加载，正在读取插件、章节源和全局策略…</p> : null}
          {auxiliaryError ? <p className="lux-error-copy" role="alert">部分编辑配置加载失败：{auxiliaryError.message}</p> : null}
          <div className="lux-library-management-toolbar">
            <span>共 {items.length} 个媒体库</span>
            <div>
              <button className="lux-library-toolbar-button" type="button" onClick={openCreate}><Plus size={16} /> 新增媒体库</button>
              <button className="lux-library-toolbar-button" type="button" onClick={() => scanAll.mutate()} disabled={scanAll.isPending}><RefreshCw size={16} /> {scanAll.isPending ? "扫描中…" : "扫描媒体库文件"}</button>
            </div>
          </div>
          {createNotice ? <p className="lux-error-copy" role="status">{createNotice}</p> : null}
          {items.length === 0 ? <div className="lux-admin-empty"><Database size={24} /><h2>还没有媒体库</h2><p>创建第一个媒体库后，Lux 才能开始索引内容。</p></div> : <div className="lux-admin-library-grid">{items.map((library) => <LibraryAdminCard key={library.id} library={library} plugins={pluginItems} chapterSources={chapterSourceItems} globalStrategy={strategy ?? undefined} />)}</div>}
        </>
      ) : (
        settings.isPending ? <AdminLibraryState title="正在加载高级配置" label="正在读取全局策略…" /> : settings.error ? <AdminLibraryState title="高级配置加载失败" label={settings.error.message} error /> : strategy ? <GlobalStrategyPanel strategy={strategy} plugins={pluginItems} saved={strategySaved} saving={saveStrategy.isPending} refreshing={refreshAll.isPending} error={saveStrategy.error?.message || refreshAll.error?.message} onChange={(next) => { setStrategySaved(false); setStrategy(next); }} onSave={(next) => saveStrategy.mutate(next)} onRefresh={(mode) => refreshAll.mutate(mode)} onBack={() => setActiveView("libraries")} /> : <AdminLibraryState title="高级配置不可用" label="全局策略为空，请稍后重试。" error />
      )}

      {createOpen ? <div className="lux-library-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setCreateOpen(false); }}>
        <div className="lux-library-dialog lux-library-create-dialog" role="dialog" aria-modal="true" aria-labelledby="new-library-title">
          <div className="lux-library-dialog-header"><div><h2 id="new-library-title">新增媒体库</h2></div><button className="lux-library-dialog-close" type="button" aria-label="关闭新增媒体库弹窗" onClick={() => setCreateOpen(false)}><X size={20} /></button></div>
          <form className="lux-admin-form lux-library-dialog-form" onSubmit={submit}>
            <label htmlFor="new-library-name">名称<input id="new-library-name" value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：电影" /></label>
            <label htmlFor="new-library-kind">类型<LuxSelect id="new-library-kind" value={kind} options={[{ value: "MOVIE", label: "电影" }, { value: "SERIES", label: "剧集" }, { value: "MIXED", label: "混合" }]} onChange={setKind} aria-label="媒体库类型" /></label>
            <ScraperListEditor value={scrapers} plugins={pluginItems} onChange={setScrapers} />
            <ChapterSourceSelect id="new-library-chapter-source" kind={kind} value={chapterSourceId} sources={chapterSourceItems} onChange={setChapterSourceId} />
            <label className="lux-admin-toggle"><input type="checkbox" aria-label="新媒体库启用实时文件监控" checked={createRealtimeWatchEnabled} onChange={(event) => setCreateRealtimeWatchEnabled(event.target.checked)} /><span>启用实时文件监控</span></label>
            <label className="lux-admin-toggle"><input type="checkbox" aria-label="新媒体库实时新增资源自动刮削" checked={createRealtimeMetadataAutoMatch} onChange={(event) => setCreateRealtimeMetadataAutoMatch(event.target.checked)} /><span>实时新增资源自动刮削</span></label>
            <section className="lux-library-dialog-section lux-library-create-root-section">
              <div className="lux-library-dialog-section-heading"><div><h3>文件夹</h3><span>创建后自动开始扫描</span></div><Folder size={18} aria-hidden="true" /></div>
              {createRootPaths.length > 0 ? <div className="lux-library-dialog-root-list">{createRootPaths.map((path) => <div className="lux-library-dialog-root-row" key={path}><span title={path}>{path}</span><button className="lux-library-dialog-icon" type="button" aria-label={`移除新媒体库路径 ${path}`} onClick={() => removeCreateRootPath(path)} disabled={create.isPending}><MinusCircle size={18} /></button></div>)}</div> : null}
              <div className="lux-library-root-form"><input id="new-library-root-path" value={createRootPath} onChange={(event) => { setCreateRootPath(event.target.value); setFormError(""); }} placeholder="输入 Docker 内的媒体路径" aria-label="新媒体库根路径" /><button className="lux-library-toolbar-button lux-library-root-browser-button" type="button" aria-label="浏览服务器目录" title="浏览服务器目录" onClick={() => setCreateDirectoryPickerOpen(true)} disabled={create.isPending}><Folder size={17} /></button></div>
              <p className="lux-library-create-root-help">可选。选择或输入路径，创建时会一起保存；可通过文件夹按钮继续添加多个文件夹。</p>
              {createDirectoryPickerOpen ? <DirectoryPicker initialPath={createRootPath.trim()} isSubmitting={create.isPending} onClose={() => setCreateDirectoryPickerOpen(false)} onSelect={addCreateRootPath} /> : null}
            </section>
            {formError ? <p className="lux-error-copy">{formError}</p> : null}
            <div className="lux-library-dialog-actions"><button className="lux-library-toolbar-button" type="button" onClick={() => setCreateOpen(false)}>取消</button><button className="lux-library-toolbar-button is-primary" type="submit" disabled={create.isPending}><Plus size={16} /> {create.isPending ? "创建中…" : "创建媒体库"}</button></div>
          </form>
        </div>
      </div> : null}
    </div>
  );
}

function GlobalStrategyPanel({
  strategy,
  plugins,
  saved,
  saving,
  refreshing,
  error,
  onChange,
  onSave,
  onRefresh,
  onBack,
}: {
  strategy: MediaStrategySettings;
  plugins: AdminPlugin[];
  saved: boolean;
  saving: boolean;
  refreshing: boolean;
  error?: string;
  onChange: (strategy: MediaStrategySettings) => void;
  onSave: (strategy: MediaStrategySettings) => void;
  onRefresh: (mode: MetadataRefreshMode) => void;
  onBack: () => void;
}) {
  const updateImages = (key: keyof MediaStrategySettings["images"], value: boolean | number) => {
    onChange({ ...strategy, images: { ...strategy.images, [key]: value } });
  };
  const updateSubtitles = (key: keyof MediaStrategySettings["subtitles"], value: boolean | string[]) => {
    onChange({ ...strategy, subtitles: { ...strategy.subtitles, [key]: value } });
  };
  const estimate = estimateStrategyStorage(strategy);

  return (
    <section className="lux-library-strategy-panel">
      <header className="lux-library-strategy-heading">
        <div>
          <h2>全局策略</h2>
          <p>给所有未单独覆盖的媒体库提供默认规则，单库可以在编辑窗口里改成自己的策略。</p>
        </div>
        <button className="lux-library-toolbar-button" type="button" onClick={onBack}>返回媒体库</button>
      </header>

      <div className="lux-library-strategy-callout">
        <Sparkles size={17} aria-hidden="true" />
        <span><strong>继承优先</strong> 新增媒体库会自动使用这里的默认值，已有媒体库的自定义覆盖不会被改写。</span>
      </div>

      <div className="lux-library-strategy-grid">
        <section className="lux-library-strategy-card lux-library-strategy-card-wide">
          <div className="lux-library-strategy-card-heading"><div><h3>元数据默认值</h3></div><Languages size={19} aria-hidden="true" /></div>
          <div className="lux-library-strategy-form-grid">
            <StrategySelect label="元数据语言" value={strategy.metadataLanguage} options={[["zh-CN", "简体中文"], ["en-US", "English"]]} onChange={(value) => onChange({ ...strategy, metadataLanguage: value })} />
            <StrategySelect label="图片语言" value={strategy.imageLanguage} options={[["zh-CN", "简体中文"], ["en", "English"], ["", "无语言偏好"]]} onChange={(value) => onChange({ ...strategy, imageLanguage: value })} />
            <StrategySelect label="认证地区" value={strategy.region} options={[["CN", "中国大陆"], ["US", "美国"], ["JP", "日本"]]} onChange={(value) => onChange({ ...strategy, region: value })} />
            <StrategySelect label="元数据刮削模式" value={strategy.metadataRefreshMode ?? "FILL_MISSING"} options={[["FILL_MISSING", "仅补全"], ["FULL_REFRESH", "完整刮削"]]} onChange={(value) => onChange({ ...strategy, metadataRefreshMode: value as MetadataRefreshMode })} />
            <ScraperSelect id="global-strategy-scraper" value={strategy.scraperId ?? ""} plugins={plugins} onChange={(value) => onChange({ ...strategy, scraperId: value || null })} />
          </div>
          <p className="lux-library-strategy-help">仅补全只写入缺失内容；完整刮削会替换已有图片，但锁定的 NFO 字段不会被替换。</p>
          <StrategyToggle label="额外保存元数据到 metadata" description="同时保存一份 NFO 和图片配置卷镜像" checked={strategy.images.writeToMetadata} onChange={(checked) => updateImages("writeToMetadata", checked)} />
        </section>

        <section className="lux-library-strategy-card lux-library-strategy-card-wide">
          <div className="lux-library-strategy-card-heading"><div><h3>图像抓取</h3></div><Image size={19} aria-hidden="true" /></div>
          <div className="lux-library-strategy-toggle-grid">
            <StrategyToggle label="海报" description="详情页和媒体库封面" checked={strategy.images.poster} onChange={(checked) => updateImages("poster", checked)} />
            <StrategyToggle label="艺术图" description="背景和横向构图" checked={strategy.images.artwork} onChange={(checked) => updateImages("artwork", checked)} />
            <StrategyToggle label="横幅图" description="宽屏入口和合集" checked={strategy.images.banner} onChange={(checked) => updateImages("banner", checked)} />
            <StrategyToggle label="徽标" description="透明标题标识" checked={strategy.images.logo} onChange={(checked) => updateImages("logo", checked)} />
            <StrategyToggle label="缩略图" description="剧集和快速浏览" checked={strategy.images.thumbnail} onChange={(checked) => updateImages("thumbnail", checked)} />
            <StrategyToggle label="光盘封面" description="光盘样式封面图" checked={strategy.images.disc} onChange={(checked) => updateImages("disc", checked)} />
            <StrategyToggle label="壁纸" description="全屏背景和详情页" checked={strategy.images.wallpaper} onChange={(checked) => updateImages("wallpaper", checked)} />
          </div>
          <div className="lux-library-strategy-form-grid lux-library-strategy-image-limits">
            <label>每项最大背景图数量<input type="number" min="0" max="20" value={strategy.images.maxBackdropCount} onChange={(event) => updateImages("maxBackdropCount", Number(event.target.value))} /></label>
            <label>最小下载宽度<input type="number" min="0" max="8192" step="128" value={strategy.images.minDownloadWidth} onChange={(event) => updateImages("minDownloadWidth", Number(event.target.value))} /><small>设为 0 表示不限制</small></label>
          </div>
        </section>

        <section className="lux-library-strategy-card">
          <div className="lux-library-strategy-card-heading"><div><h3>字幕默认值</h3></div><Languages size={19} aria-hidden="true" /></div>
          <div className="lux-library-strategy-toggle-list">
            <StrategyToggle label="自动下载字幕" checked={strategy.subtitles.autoDownload} onChange={(checked) => updateSubtitles("autoDownload", checked)} />
            <StrategyToggle label="仅下载强制字幕" checked={strategy.subtitles.forcedOnly} onChange={(checked) => updateSubtitles("forcedOnly", checked)} />
            <StrategyToggle label="包含听障字幕" checked={strategy.subtitles.hearingImpaired} onChange={(checked) => updateSubtitles("hearingImpaired", checked)} />
          </div>
          <label className="lux-library-strategy-language-input">默认语言<input value={strategy.subtitles.languages.join(", ")} onChange={(event) => updateSubtitles("languages", event.target.value.split(",").map((value) => value.trim()).filter(Boolean).slice(0, 8))} placeholder="zh-CN, en" /><small>使用逗号分隔，最多 8 种语言。</small></label>
        </section>

        <section className="lux-library-strategy-card">
          <div className="lux-library-strategy-card-heading"><div><h3>存储预估</h3></div><HardDrive size={19} aria-hidden="true" /></div>
          <div className="lux-library-strategy-estimate"><strong>{estimate.storage}</strong><span>每 10,000 个条目</span><small>约 {estimate.imagesPerItem} 张图片 / 条目，实际大小取决于来源和格式。</small></div>
          <label className="lux-library-strategy-scope">应用范围<LuxSelect value={strategy.applyScope} options={[{ value: "NEW_CONTENT", label: "仅新内容" }, { value: "SELECTED_CONTENT", label: "刷新选中内容" }, { value: "ALL_CONTENT", label: "后台刷新全部内容" }]} onChange={(applyScope) => onChange({ ...strategy, applyScope })} aria-label="策略应用范围" /><small>{scopeDescription(strategy.applyScope)}</small></label>
        </section>
      </div>

      <footer className="lux-library-strategy-footer">
        <span>{saved ? "全局策略已保存" : "保存后，新内容会按此策略处理。"}{error ? ` ${error}` : ""}</span>
        <div className="lux-library-strategy-footer-actions">
          <button className="lux-library-toolbar-button" type="button" onClick={() => onRefresh((strategy.metadataRefreshMode ?? "FILL_MISSING") as MetadataRefreshMode)} disabled={refreshing}><RefreshCw size={15} /> {refreshing ? "提交中…" : "开始全局刮削"}</button>
          <button className="lux-library-toolbar-button is-primary" type="button" onClick={() => onSave(strategy)} disabled={saving}><Save size={15} /> {saving ? "保存中…" : "保存全局策略"}</button>
        </div>
      </footer>
    </section>
  );
}

function StrategyToggle({ label, description, checked, onChange }: { label: string; description?: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return <label className="lux-library-strategy-toggle"><input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)} /><span><strong>{label}</strong>{description ? <small>{description}</small> : null}</span><i aria-hidden="true" /></label>;
}

function StrategySelect({ label, value, options, onChange }: { label: string; value: string; options: Array<[string, string]>; onChange: (value: string) => void }) {
  return <label className="lux-library-strategy-select">{label}<LuxSelect value={value} options={options.map(([optionValue, optionLabel]) => ({ value: optionValue, label: optionLabel }))} onChange={onChange} aria-label={label} /></label>;
}

function estimateStrategyStorage(strategy: MediaStrategySettings) {
  const enabledTypes = [strategy.images.poster, strategy.images.artwork, strategy.images.banner, strategy.images.logo, strategy.images.thumbnail, strategy.images.disc, strategy.images.wallpaper].filter(Boolean).length;
  const extraBackdrops = strategy.images.artwork || strategy.images.banner || strategy.images.wallpaper ? Math.max(0, strategy.images.maxBackdropCount - 1) : 0;
  const imagesPerItem = enabledTypes + extraBackdrops;
  return { imagesPerItem, storage: `${Math.max(0.1, imagesPerItem * 1.8).toFixed(1)} GB` };
}

function scopeDescription(scope: string) {
  if (scope === "ALL_CONTENT") return "后续刷新全部内容时进入后台任务队列。";
  if (scope === "SELECTED_CONTENT") return "后续手动刷新时只处理选中的条目。";
  return "只影响之后入库的新内容，不主动刷新现有内容。";
}

function LibraryAdminCard({ library, plugins, chapterSources, globalStrategy }: { library: AdminLibrary; plugins: AdminPlugin[]; chapterSources: ChapterSource[]; globalStrategy?: MediaStrategySettings }) {
  const queryClient = useQueryClient();
  const menuRef = useRef<HTMLDivElement>(null);
  const dialogCloseRef = useRef<HTMLButtonElement>(null);
  const coverInputRef = useRef<HTMLInputElement>(null);
  const [rootPath, setRootPath] = useState("");
  const [rootError, setRootError] = useState("");
  const [directoryPickerOpen, setDirectoryPickerOpen] = useState(false);
  const [scrapers, setScrapers] = useState(() => normalizeLibraryScrapers(library));
  const [chapterSourceId, setChapterSourceId] = useState(library.chapterSourceId ?? "");
  const [menuOpen, setMenuOpen] = useState(false);
  const [editOpen, setEditOpen] = useState(false);
  const [editName, setEditName] = useState(library.name);
  const [editKind, setEditKind] = useState(library.kind);
  const [editError, setEditError] = useState("");
  const [coverError, setCoverError] = useState("");
  const [removeError, setRemoveError] = useState("");
  const update = useMutation({
    mutationFn: ({ values }: { values: Record<string, unknown>; close?: boolean }) => api.updateAdminLibrary(library.id, values),
    onSuccess: (_data, variables) => {
      if (variables.close) setEditOpen(false);
      setEditError("");
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const uploadCover = useMutation({ mutationFn: (file: File) => api.updateAdminLibraryCover(library.id, file), onSuccess: () => { setCoverError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); }, onError: (error) => setCoverError(error.message) });
  const addRoot = useMutation({ mutationFn: (path: string) => api.addAdminLibraryRoot(library.id, path.trim()), onSuccess: () => { setRootPath(""); setRootError(""); setDirectoryPickerOpen(false); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); }, onError: (error) => setRootError(error.message) });
  const removeRoot = useMutation({ mutationFn: (rootId: string) => api.deleteAdminLibraryRoot(library.id, rootId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }) });
  const scan = useMutation({ mutationFn: () => api.startAdminScan(library.id), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminHealth }); void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() }); } });
  const refresh = useMutation({
    mutationFn: () => api.startLibraryMetadataRefresh(library.id, library.mediaStrategy?.metadataRefreshMode ?? globalStrategy?.metadataRefreshMode ?? "FILL_MISSING"),
    onSuccess: () => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() }); },
  });
  const remove = useMutation({
    mutationFn: () => api.deleteAdminLibrary(library.id),
    onSuccess: () => {
      setRemoveError("");
      queryClient.setQueryData<{ libraries?: AdminLibrary[] }>(queryKeys.adminLibraries, (data) => data
        ? { ...data, libraries: (data.libraries ?? []).filter((item) => item.id !== library.id) }
        : data);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
    onError: (error) => setRemoveError(error.message || "删除媒体库失败，请重试"),
  });

  useEffect(() => {
    if (!menuOpen) return undefined;
    const closeMenu = (event: MouseEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    document.addEventListener("mousedown", closeMenu);
    return () => document.removeEventListener("mousedown", closeMenu);
  }, [menuOpen]);

  useEffect(() => {
    if (editOpen) dialogCloseRef.current?.focus();
  }, [editOpen]);

  const openEdit = () => {
    setMenuOpen(false);
    setEditOpen(true);
    setEditName(library.name);
    setEditKind(library.kind);
    setScrapers(normalizeLibraryScrapers(library));
    setChapterSourceId(library.chapterSourceId ?? "");
  };
  const updateScrapers = (value: AdminLibraryScraper[]) => {
    setScrapers(value);
    update.mutate({ values: { scrapers: value.map(({ scraperId, role }) => ({ scraperId, role })) } });
  };
  const updateChapterSource = (value: string) => { setChapterSourceId(value); update.mutate({ values: { chapterSourceId: value || null } }); };
  const saveDetails = (event: FormEvent) => {
    event.preventDefault();
    const nextName = editName.trim();
    if (!nextName) {
      setEditError("请输入媒体库名称");
      return;
    }
    setEditError("");
    update.mutate({ values: { name: nextName, kind: editKind, chapterSourceId: supportsChapterSource(editKind) ? chapterSourceId || null : null }, close: true });
  };
  const selectCover = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) uploadCover.mutate(file);
  };
  const deleteLibrary = () => {
    setMenuOpen(false);
    if (window.confirm(`确定删除媒体库“${library.name}”？`)) {
      setRemoveError("");
      remove.mutate();
    }
  };

  return (
    <article className="lux-admin-library-card" ref={menuRef}>
      <div className="lux-admin-library-cover-wrap">
        {library.coverImageUrl ? <img className="lux-admin-library-cover" src={library.coverImageUrl} alt={`${library.name} 封面`} /> : <div className="lux-admin-library-cover-placeholder"><Image size={30} aria-hidden="true" /></div>}
        <button className="lux-admin-library-overflow" type="button" aria-label={`打开 ${library.name} 操作菜单`} aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen((open) => !open)}><MoreHorizontal size={20} /></button>
      </div>
      <div className="lux-admin-library-copy"><strong>{library.name}</strong><span>{libraryKindLabel(library.kind)}</span><small>{library.roots.length > 1 ? `${library.roots.length}个文件夹` : library.roots[0]?.displayPath ?? "尚未配置根路径"}</small></div>
      {removeError ? <p className="lux-error-copy" role="alert">{removeError}</p> : null}
      {menuOpen ? <LibraryActionMenu library={library} onEdit={openEdit} onRefresh={() => { setMenuOpen(false); refresh.mutate(); }} refreshing={refresh.isPending} onScan={() => { setMenuOpen(false); scan.mutate(); }} onRemove={deleteLibrary} /> : null}
      {editOpen ? <div className="lux-library-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setEditOpen(false); }} onKeyDown={(event) => { if (event.key === "Escape") setEditOpen(false); }}>
        <div className="lux-library-dialog" role="dialog" aria-modal="true" aria-labelledby={`edit-library-title-${library.id}`}>
          <div className="lux-library-dialog-header"><div><h2 id={`edit-library-title-${library.id}`}>{library.name}</h2></div><button ref={dialogCloseRef} className="lux-library-dialog-close" type="button" aria-label={`关闭 ${library.name} 编辑弹窗`} onClick={() => setEditOpen(false)}><X size={20} /></button></div>
          <div className="lux-library-dialog-scroll">
            <p className="lux-library-warning">如果更改了元数据或媒体图片下载的设置，只适用于之后添加到媒体库的新内容。要将更改应用于现有项目，您需要手动刷新其元数据。</p>
            <section className="lux-library-dialog-section"><div className="lux-library-dialog-section-heading"><h3>文件夹</h3><button className="lux-library-toolbar-button" type="button" onClick={() => document.getElementById(`library-root-path-${library.id}`)?.focus()}><Plus size={16} /> 添加</button></div>{library.roots.length === 0 ? <p className="lux-admin-muted">尚未配置根路径。</p> : <div className="lux-library-dialog-root-list">{library.roots.map((root) => <div className="lux-library-dialog-root-row" key={root.id}><span title={root.displayPath}>{root.displayPath}</span><button className="lux-library-dialog-icon" type="button" aria-label={`编辑路径 ${root.displayPath}`} disabled><Pencil size={17} /></button><button className="lux-library-dialog-icon" type="button" aria-label={`删除路径 ${root.displayPath}`} onClick={() => removeRoot.mutate(root.id)} disabled={removeRoot.isPending}><MinusCircle size={18} /></button></div>)}</div>}<div className="lux-library-root-form"><input id={`library-root-path-${library.id}`} value={rootPath} onChange={(event) => setRootPath(event.target.value)} placeholder="输入 Docker 内的媒体路径" aria-label={`${library.name} 新根路径`} /><button className="lux-library-toolbar-button lux-library-root-browser-button" type="button" aria-label="浏览服务器目录" title="浏览服务器目录" onClick={() => setDirectoryPickerOpen(true)}><Folder size={17} /></button></div>{directoryPickerOpen ? <DirectoryPicker initialPath={rootPath.trim()} isSubmitting={addRoot.isPending} onClose={() => setDirectoryPickerOpen(false)} onSelect={(path) => { setRootPath(path); setRootError(""); addRoot.mutate(path); }} /> : null}{rootError ? <p className="lux-error-copy">{rootError}</p> : null}</section>
            <form className="lux-library-dialog-section lux-library-settings-form" onSubmit={saveDetails}><h3>媒体库设置</h3><label htmlFor={`library-name-${library.id}`}>媒体库名称<input id={`library-name-${library.id}`} aria-label={`${library.name} 媒体库名称`} value={editName} onChange={(event) => setEditName(event.target.value)} /></label><label htmlFor={`library-kind-${library.id}`}>媒体库类型<LuxSelect id={`library-kind-${library.id}`} value={editKind} options={[{ value: "MOVIE", label: "电影" }, { value: "SERIES", label: "剧集" }, { value: "MIXED", label: "混合" }]} onChange={setEditKind} aria-label={`${library.name} 媒体库类型`} /></label><label className="lux-admin-toggle"><input type="checkbox" checked={library.isEnabled} onChange={(event) => update.mutate({ values: { isEnabled: event.target.checked } })} /><span>启用媒体库</span></label><label className="lux-admin-toggle"><input type="checkbox" aria-label={`${library.name} 启用实时文件监控`} checked={library.realtimeWatchEnabled} onChange={(event) => update.mutate({ values: { realtimeWatchEnabled: event.target.checked } })} /><span>启用实时文件监控</span></label><label className="lux-admin-toggle"><input type="checkbox" aria-label={`${library.name} 实时新增资源自动刮削`} checked={library.realtimeMetadataAutoMatchEnabled} onChange={(event) => update.mutate({ values: { realtimeMetadataAutoMatchEnabled: event.target.checked } })} /><span>实时新增资源自动刮削</span></label><ScraperListEditor value={scrapers} plugins={plugins} onChange={updateScrapers} /><ChapterSourceSelect id={`library-chapter-source-${library.id}`} kind={editKind} value={chapterSourceId} sources={chapterSources} onChange={updateChapterSource} />{editError ? <p className="lux-error-copy">{editError}</p> : null}<div className="lux-library-dialog-actions"><button className="lux-library-toolbar-button is-primary" type="submit" disabled={update.isPending}><Save size={15} /> 保存修改</button><button className="lux-library-toolbar-button" type="button" onClick={() => setEditOpen(false)}>取消</button></div></form>
            {globalStrategy ? <LibraryStrategyOverride library={library} globalStrategy={globalStrategy} onSave={(value) => update.mutateAsync({ values: { mediaStrategy: value } })} /> : null}
            <section className="lux-library-dialog-section"><div className="lux-library-dialog-section-heading"><h3>媒体库图像</h3><span>JPEG、PNG、WebP，最大 5 MiB</span></div><div className="lux-library-cover-editor"><div className="lux-library-cover-preview">{library.coverImageUrl ? <img src={library.coverImageUrl} alt="" /> : <Image size={24} aria-hidden="true" />}</div><input ref={coverInputRef} className="lux-visually-hidden" type="file" accept="image/jpeg,image/png,image/webp" aria-label={`${library.name} 封面图片`} onChange={selectCover} /><button className="lux-library-toolbar-button" type="button" onClick={() => coverInputRef.current?.click()} disabled={uploadCover.isPending}><Upload size={15} /> {uploadCover.isPending ? "上传中…" : library.coverImageUrl ? "替换封面" : "上传封面"}</button>{coverError ? <p className="lux-error-copy">{coverError}</p> : null}</div></section>
          </div>
        </div>
      </div> : null}
    </article>
  );
}

function LibraryStrategyOverride({ library, globalStrategy, onSave }: { library: AdminLibrary; globalStrategy: MediaStrategySettings; onSave: (strategy: MediaStrategySettings | null) => Promise<unknown> }) {
  const [mode, setMode] = useState<"inherit" | "custom">(library.mediaStrategy ? "custom" : "inherit");
  const [draft, setDraft] = useState<MediaStrategySettings>(() => cloneStrategy(library.mediaStrategy ?? globalStrategy));
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState("");
  const updateImages = (key: keyof MediaStrategySettings["images"], value: boolean | number) => setDraft((current) => ({ ...current, images: { ...current.images, [key]: value } }));
  const save = async () => {
    setError("");
    try {
      await onSave(mode === "custom" ? draft : null);
      setSaved(true);
    } catch (saveError) {
      setSaved(false);
      setError(saveError instanceof Error ? saveError.message : "保存失败");
    }
  };
  return <section className="lux-library-dialog-section lux-library-override-section">
    <div className="lux-library-dialog-section-heading"><div><h3>媒体库策略</h3><span>{mode === "custom" ? "此库使用自定义覆盖" : "此库继承全局策略"}</span></div><Settings2 size={18} aria-hidden="true" /></div>
    <div className="lux-library-override-modes" role="radiogroup" aria-label={`${library.name} 媒体库策略来源`}>
      <label><input type="radio" name={`strategy-mode-${library.id}`} checked={mode === "inherit"} onChange={() => { setMode("inherit"); setSaved(false); }} /><span><strong>继承全局</strong><small>跟随全局策略的语言、图片和字幕默认值。</small></span></label>
      <label><input type="radio" name={`strategy-mode-${library.id}`} checked={mode === "custom"} onChange={() => { setMode("custom"); setSaved(false); }} /><span><strong>自定义覆盖</strong><small>只对这个媒体库使用独立的内容策略。</small></span></label>
    </div>
    {mode === "custom" ? <>
      <div className="lux-library-override-toggles">
        <StrategyToggle label="海报" checked={draft.images.poster} onChange={(checked) => updateImages("poster", checked)} />
        <StrategyToggle label="艺术图" checked={draft.images.artwork} onChange={(checked) => updateImages("artwork", checked)} />
        <StrategyToggle label="横幅图" checked={draft.images.banner} onChange={(checked) => updateImages("banner", checked)} />
        <StrategyToggle label="徽标" checked={draft.images.logo} onChange={(checked) => updateImages("logo", checked)} />
        <StrategyToggle label="缩略图" checked={draft.images.thumbnail} onChange={(checked) => updateImages("thumbnail", checked)} />
        <StrategyToggle label="光盘封面" checked={draft.images.disc} onChange={(checked) => updateImages("disc", checked)} />
        <StrategyToggle label="壁纸" checked={draft.images.wallpaper} onChange={(checked) => updateImages("wallpaper", checked)} />
        <StrategyToggle label="额外保存元数据到 metadata" checked={draft.images.writeToMetadata} onChange={(checked) => updateImages("writeToMetadata", checked)} />
      </div>
      <div className="lux-library-override-fields">
        <StrategySelect label="元数据语言" value={draft.metadataLanguage} options={[["zh-CN", "简体中文"], ["en-US", "English"], ["ja-JP", "日本語"]]} onChange={(value) => setDraft((current) => ({ ...current, metadataLanguage: value }))} />
        <label>最大背景图数量<input type="number" min="0" max="20" value={draft.images.maxBackdropCount} onChange={(event) => updateImages("maxBackdropCount", Number(event.target.value))} /></label>
        <label>最小下载宽度<input type="number" min="0" max="8192" step="128" value={draft.images.minDownloadWidth} onChange={(event) => updateImages("minDownloadWidth", Number(event.target.value))} /></label>
      </div>
    </> : <p className="lux-admin-muted lux-library-override-inherit-copy">当前使用全局默认：{globalStrategy.metadataLanguage} · {globalStrategy.images.minDownloadWidth || "不限"} px · {globalStrategy.images.maxBackdropCount} 张背景图。</p>}
    {error ? <p className="lux-error-copy">{error}</p> : null}
    <div className="lux-library-override-actions"><span>{saved ? "策略覆盖已保存" : "覆盖只影响当前媒体库"}</span><button className="lux-library-toolbar-button" type="button" onClick={() => void save()} disabled={saved && mode === "inherit"}><Save size={14} /> {saved ? "已保存" : "保存策略"}</button></div>
  </section>;
}

function cloneStrategy(strategy: MediaStrategySettings): MediaStrategySettings {
  return { ...strategy, images: { ...strategy.images }, subtitles: { ...strategy.subtitles, languages: [...strategy.subtitles.languages] } };
}

function LibraryActionMenu({ library, onEdit, onRefresh, refreshing, onScan, onRemove }: { library: AdminLibrary; onEdit: () => void; onRefresh: () => void; refreshing: boolean; onScan: () => void; onRemove: () => void }) {
  const actions: Array<{ label: string; icon: LucideIcon; onClick?: () => void; disabled?: boolean; title?: string }> = [
    { label: "添加到“合集”", icon: ListPlus, disabled: true, title: "合集功能将在后续版本提供" },
    { label: "更改内容类型", icon: Folder, onClick: onEdit },
    { label: "编辑", icon: Pencil, onClick: onEdit },
    { label: "编辑图像", icon: Image, onClick: onEdit },
    { label: "刷新元数据", icon: RefreshCw, onClick: onRefresh, disabled: refreshing, title: refreshing ? "元数据刷新任务提交中" : undefined },
    { label: "扫描媒体库文件", icon: RefreshCw, onClick: onScan },
    { label: "移除", icon: MinusCircle, onClick: onRemove },
    { label: "重命名", icon: Pencil, onClick: onEdit },
    { label: "复制", icon: Copy, disabled: true, title: "复制媒体库功能暂未启用" },
  ];
  return <div className="lux-library-action-menu" role="menu" aria-label={`${library.name} 操作`}><div className="lux-library-action-menu-heading">{library.coverImageUrl ? <img src={library.coverImageUrl} alt="" /> : <span><Database size={16} /></span>}<strong>{library.name}</strong></div>{actions.map(({ label, icon: Icon, onClick, disabled, title }, index) => <span key={label}>{index === 6 ? <i className="lux-library-action-divider" aria-hidden="true" /> : null}<button type="button" role="menuitem" onClick={onClick} disabled={disabled} title={title}><Icon size={18} /><span>{label}</span></button></span>)}</div>;
}

function normalizeLibraryScrapers(library: AdminLibrary): AdminLibraryScraper[] {
  const configured = library.scrapers?.length
    ? library.scrapers
    : library.scraperId
      ? [{ scraperId: library.scraperId, position: 0, role: "PRIMARY" }]
      : [];
  return configured
    .slice()
    .sort((left, right) => left.position - right.position)
    .map((scraper, position) => ({
      scraperId: scraper.scraperId,
      position,
      role: position === 0 ? "PRIMARY" : scraper.role === "PRIMARY" ? "BACKUP" : scraper.role,
    }));
}

function ScraperListEditor({ value, plugins, onChange }: { value: AdminLibraryScraper[]; plugins: AdminPlugin[]; onChange: (value: AdminLibraryScraper[]) => void }) {
  const [dragging, setDragging] = useState<number | null>(null);
  const selected = new Set(value.map((scraper) => scraper.scraperId));
  const availablePlugins = plugins.filter((plugin) => plugin.category.trim().toUpperCase() === "SCRAPER");
  const options = availablePlugins
    .filter((plugin) => plugin.available && !selected.has(plugin.id))
    .map((plugin) => ({ value: plugin.id, label: plugin.name }));
  const reorder = (from: number, to: number) => {
    if (from === to || from < 0 || to < 0 || from >= value.length || to >= value.length) return;
    const next = value.slice();
    const [moved] = next.splice(from, 1);
    next.splice(to, 0, moved);
    onChange(next.map((scraper, position) => ({ ...scraper, position, role: position === 0 ? "PRIMARY" : scraper.role === "PRIMARY" ? "BACKUP" : scraper.role })));
  };
  const add = (scraperId: string) => {
    if (!scraperId || selected.has(scraperId)) return;
    onChange([...value, { scraperId, position: value.length, role: value.length === 0 ? "PRIMARY" : "BACKUP" }]);
  };
  const updateRole = (position: number, role: string) => {
    if (position === 0) return;
    onChange(value.map((scraper, index) => index === position ? { ...scraper, role } : scraper));
  };
  const pluginName = (scraperId: string) => {
    const plugin = plugins.find((item) => item.id === scraperId);
    return plugin ? `${plugin.name}${plugin.available ? "" : "（暂不可用）"}` : `${scraperId}（暂不可用）`;
  };
  return <div className="lux-admin-scraper-field lux-admin-scraper-list-field">
    <span>刮削器顺序</span>
    <small>首位负责主匹配；后续来源可作为补充、备用或两者兼具。</small>
    <LuxSelect id="library-scraper-add" value="" options={options} placeholder={value.length ? "添加刮削器" : "选择首个刮削器"} disabled={options.length === 0} onChange={add} aria-label="刮削器" />
    {value.length > 0 ? <div className="lux-admin-scraper-list" aria-label="已选刮削器列表">
      {value.map((scraper, index) => <div
        className="lux-admin-scraper-row"
        key={scraper.scraperId}
        draggable
        onDragStart={() => setDragging(index)}
        onDragOver={(event: DragEvent<HTMLDivElement>) => event.preventDefault()}
        onDrop={() => { if (dragging !== null) reorder(dragging, index); setDragging(null); }}
      >
        <span className="lux-admin-scraper-position">{index + 1}</span>
        <span className="lux-admin-scraper-name" title={scraper.scraperId}>{pluginName(scraper.scraperId)}</span>
        {index === 0 ? <span className="lux-admin-scraper-role">主刮削器</span> : <LuxSelect id={`library-scraper-role-${index}`} value={scraper.role} options={[{ value: "SUPPLEMENT", label: "补充" }, { value: "BACKUP", label: "备用" }, { value: "BOTH", label: "补充 + 备用" }]} onChange={(role) => updateRole(index, role)} aria-label={`刮削器 ${pluginName(scraper.scraperId)} 角色`} />}
        <div className="lux-admin-scraper-actions" role="group" aria-label="刮削器操作">
          <button className="lux-library-dialog-icon" type="button" aria-label={`上移刮削器 ${pluginName(scraper.scraperId)}`} disabled={index === 0} onClick={() => reorder(index, index - 1)}>↑</button>
          <button className="lux-library-dialog-icon" type="button" aria-label={`下移刮削器 ${pluginName(scraper.scraperId)}`} disabled={index === value.length - 1} onClick={() => reorder(index, index + 1)}>↓</button>
          <button className="lux-library-dialog-icon" type="button" aria-label={`移除刮削器 ${pluginName(scraper.scraperId)}`} onClick={() => onChange(value.filter((_, itemIndex) => itemIndex !== index).map((item, position) => ({ ...item, position, role: position === 0 ? "PRIMARY" : item.role === "PRIMARY" ? "BACKUP" : item.role })))}>×</button>
        </div>
      </div>)}
    </div> : <p className="lux-admin-muted">未配置在线刮削器。</p>}
    {value.length > 0 ? <button className="lux-library-dialog-icon lux-admin-scraper-clear" type="button" aria-label="清除刮削器配置" onClick={() => onChange([])}>清除全部刮削器</button> : null}
  </div>;
}

function ScraperSelect({ id, value, plugins, onChange }: { id: string; value: string; plugins: AdminPlugin[]; onChange: (value: string) => void }) {
  const visiblePlugins = plugins.filter((plugin) => plugin.category.trim().toUpperCase() === "SCRAPER" && (plugin.available || plugin.id === value));
  const options = visiblePlugins.map((plugin) => ({
    value: plugin.id,
    label: `${plugin.name}${plugin.available ? "" : "（暂不可用）"}`,
    disabled: !plugin.available,
  }));
  return <label className="lux-admin-scraper-field" htmlFor={id}>
    刮削器
    <LuxSelect id={id} value={value} options={options} placeholder="未配置刮削器" disabled={options.length === 0} onChange={onChange} aria-label="刮削器" />
    {value ? <button className="lux-library-dialog-icon" type="button" aria-label="清除刮削器配置" onClick={() => onChange("")}>清除配置</button> : null}
  </label>;
}

function ChapterSourceSelect({ id, kind, value, sources, onChange }: { id: string; kind: string; value: string; sources: ChapterSource[]; onChange: (value: string) => void }) {
  const supported = supportsChapterSource(kind);
  const options = [
    ...sources.map((source) => ({ value: source.id, label: `${source.name}${chapterSourceKindLabel(source.supportedMediaSourceKinds)}` })),
    ...(value && !sources.some((source) => source.id === value)
      ? [{ value, label: "已配置来源（暂不可用）", disabled: true }]
      : []),
  ];
  return <label className="lux-admin-scraper-field" htmlFor={id}>
    片头片尾数据源
    <LuxSelect id={id} value={supported ? value : ""} options={options} placeholder={supported ? "不使用片头片尾数据源" : "仅剧集或混合媒体库可用"} disabled={!supported || options.length === 0} onChange={onChange} aria-label="片头片尾数据源" />
    {value ? <button className="lux-library-dialog-icon" type="button" aria-label="清除片头片尾数据源配置" onClick={() => onChange("")}>清除配置</button> : null}
    <small>{supported ? "选择后，该媒体库的检测任务和章节输出只使用此来源。" : "电影库不支持片头片尾数据源。"}</small>
  </label>;
}

function chapterSourceKindLabel(kinds: string[] | undefined) {
  const labels = (kinds ?? []).map((kind) => {
    if (kind === "LOCAL_FILE") return "本地文件";
    if (kind === "STRM_URL") return ".strm";
    return kind;
  }).filter(Boolean);
  return labels.length > 0 ? `（${labels.join("、")}）` : "（未声明媒体类型）";
}

function supportsChapterSource(kind: string) {
  return kind === "SERIES" || kind === "MIXED";
}

function libraryKindLabel(kind: string) {
  if (kind === "MOVIE") return "影片";
  if (kind === "SERIES") return "电视剧";
  return "混合内容";
}

function AdminLibraryState({ title, label, error = false }: { title?: string; label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><h1>{title ?? (error ? "媒体库加载失败" : "正在加载媒体库")}</h1><p>{label}</p></section>;
}
