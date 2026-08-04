import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Copy,
  Database,
  Folder,
  FolderPlus,
  Image,
  ListPlus,
  MinusCircle,
  MoreHorizontal,
  Pencil,
  Plus,
  RefreshCw,
  Save,
  Upload,
  X,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { ChangeEvent, FormEvent, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminLibrary, AdminPlugin } from "../../lib/api/types";

export function AdminLibrariesPage() {
  const queryClient = useQueryClient();
  const libraries = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const plugins = useQuery({ queryKey: queryKeys.adminPlugins, queryFn: () => api.adminPlugins() });
  const [activeView, setActiveView] = useState<"libraries" | "advanced">("libraries");
  const [createOpen, setCreateOpen] = useState(false);
  const [name, setName] = useState("");
  const [kind, setKind] = useState("MOVIE");
  const [watchEnabled, setWatchEnabled] = useState(true);
  const [scraperId, setScraperId] = useState("");
  const [formError, setFormError] = useState("");
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
  const create = useMutation({
    mutationFn: () => api.createAdminLibrary({ name: name.trim(), kind, realtimeWatchEnabled: watchEnabled, scraperId: scraperId || null }),
    onSuccess: () => {
      setName("");
      setScraperId("");
      setFormError("");
      setCreateOpen(false);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
    onError: (error) => setFormError(error.message),
  });

  if (libraries.isPending || plugins.isPending) return <AdminLibraryState label="正在读取媒体库与插件…" />;
  if (libraries.error || plugins.error) return <AdminLibraryState label={libraries.error?.message || plugins.error?.message || "管理数据加载失败"} error />;

  const items = libraries.data.libraries ?? [];
  const pluginItems = plugins.data.plugins ?? [];
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) {
      setFormError("请输入媒体库名称");
      return;
    }
    create.mutate();
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
          <div className="lux-library-management-toolbar">
            <span>共 {items.length} 个媒体库</span>
            <div>
              <button className="lux-library-toolbar-button" type="button" onClick={() => setCreateOpen(true)}><Plus size={16} /> 新增媒体库</button>
              <button className="lux-library-toolbar-button" type="button" onClick={() => scanAll.mutate()} disabled={scanAll.isPending}><RefreshCw size={16} /> {scanAll.isPending ? "扫描中…" : "扫描媒体库文件"}</button>
            </div>
          </div>
          {items.length === 0 ? <div className="lux-admin-empty"><Database size={24} /><h2>还没有媒体库</h2><p>创建第一个媒体库后，Lux 才能开始索引内容。</p></div> : <div className="lux-admin-library-grid">{items.map((library) => <LibraryAdminCard key={library.id} library={library} plugins={pluginItems} />)}</div>}
        </>
      ) : (
        <section className="lux-library-advanced-panel">
          <span className="lux-eyebrow">ADVANCED SETTINGS</span>
          <h2>高级设置</h2>
          <p>扫描计划、元数据计划和实时监听等设置，已移动到每个媒体库的编辑窗口中。</p>
          <button className="lux-library-toolbar-button" type="button" onClick={() => setActiveView("libraries")}>返回媒体库</button>
        </section>
      )}

      {createOpen ? <div className="lux-library-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setCreateOpen(false); }}>
        <div className="lux-library-dialog lux-library-create-dialog" role="dialog" aria-modal="true" aria-labelledby="new-library-title">
          <div className="lux-library-dialog-header"><div><span className="lux-eyebrow">NEW LIBRARY</span><h2 id="new-library-title">新增媒体库</h2></div><button className="lux-library-dialog-close" type="button" aria-label="关闭新增媒体库弹窗" onClick={() => setCreateOpen(false)}><X size={20} /></button></div>
          <form className="lux-admin-form lux-library-dialog-form" onSubmit={submit}>
            <label htmlFor="new-library-name">名称<input id="new-library-name" value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：电影" /></label>
            <label htmlFor="new-library-kind">类型<select id="new-library-kind" value={kind} onChange={(event) => setKind(event.target.value)}><option value="MOVIE">电影</option><option value="SERIES">剧集</option><option value="MIXED">混合</option></select></label>
            <ScraperSelect id="new-library-scraper" value={scraperId} plugins={pluginItems} onChange={setScraperId} />
            <label className="lux-admin-toggle"><input type="checkbox" checked={watchEnabled} onChange={(event) => setWatchEnabled(event.target.checked)} /><span>启用实时监听</span></label>
            {formError ? <p className="lux-error-copy">{formError}</p> : null}
            <div className="lux-library-dialog-actions"><button className="lux-library-toolbar-button" type="button" onClick={() => setCreateOpen(false)}>取消</button><button className="lux-library-toolbar-button is-primary" type="submit" disabled={create.isPending}><Plus size={16} /> {create.isPending ? "创建中…" : "创建媒体库"}</button></div>
          </form>
        </div>
      </div> : null}
    </div>
  );
}

function LibraryAdminCard({ library, plugins }: { library: AdminLibrary; plugins: AdminPlugin[] }) {
  const queryClient = useQueryClient();
  const menuRef = useRef<HTMLDivElement>(null);
  const dialogCloseRef = useRef<HTMLButtonElement>(null);
  const coverInputRef = useRef<HTMLInputElement>(null);
  const [rootPath, setRootPath] = useState("");
  const [rootError, setRootError] = useState("");
  const [scraperId, setScraperId] = useState(library.scraperId ?? "");
  const [menuOpen, setMenuOpen] = useState(false);
  const [editOpen, setEditOpen] = useState(false);
  const [editName, setEditName] = useState(library.name);
  const [editKind, setEditKind] = useState(library.kind);
  const [editError, setEditError] = useState("");
  const [coverError, setCoverError] = useState("");
  const update = useMutation({
    mutationFn: ({ values }: { values: Record<string, unknown>; close?: boolean }) => api.updateAdminLibrary(library.id, values),
    onSuccess: (_data, variables) => {
      if (variables.close) setEditOpen(false);
      setEditError("");
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries });
    },
  });
  const uploadCover = useMutation({ mutationFn: (file: File) => api.updateAdminLibraryCover(library.id, file), onSuccess: () => { setCoverError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); }, onError: (error) => setCoverError(error.message) });
  const addRoot = useMutation({ mutationFn: () => api.addAdminLibraryRoot(library.id, rootPath.trim()), onSuccess: () => { setRootPath(""); setRootError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); }, onError: (error) => setRootError(error.message) });
  const removeRoot = useMutation({ mutationFn: (rootId: string) => api.deleteAdminLibraryRoot(library.id, rootId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }) });
  const scan = useMutation({ mutationFn: () => api.startAdminScan(library.id), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminHealth }); void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() }); } });
  const remove = useMutation({ mutationFn: () => api.deleteAdminLibrary(library.id), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }) });

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
  };
  const updateSchedule = (key: string, value: string) => update.mutate({ values: { [key]: value || null } });
  const updateScraper = (value: string) => { setScraperId(value); update.mutate({ values: { scraperId: value || null } }); };
  const saveDetails = (event: FormEvent) => {
    event.preventDefault();
    const nextName = editName.trim();
    if (!nextName) {
      setEditError("请输入媒体库名称");
      return;
    }
    setEditError("");
    update.mutate({ values: { name: nextName, kind: editKind }, close: true });
  };
  const selectCover = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) uploadCover.mutate(file);
  };
  const deleteLibrary = () => {
    setMenuOpen(false);
    if (window.confirm(`确定删除媒体库“${library.name}”？`)) remove.mutate();
  };

  return (
    <article className="lux-admin-library-card" ref={menuRef}>
      <div className="lux-admin-library-cover-wrap">
        {library.coverImageUrl ? <img className="lux-admin-library-cover" src={library.coverImageUrl} alt={`${library.name} 封面`} /> : <div className="lux-admin-library-cover-placeholder"><Image size={30} aria-hidden="true" /></div>}
        <button className="lux-admin-library-overflow" type="button" aria-label={`打开 ${library.name} 操作菜单`} aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen((open) => !open)}><MoreHorizontal size={20} /></button>
      </div>
      <div className="lux-admin-library-copy"><strong>{library.name}</strong><span>{libraryKindLabel(library.kind)}</span><small>{library.roots[0]?.displayPath ?? "尚未配置根路径"}</small></div>
      {menuOpen ? <LibraryActionMenu library={library} onEdit={openEdit} onScan={() => { setMenuOpen(false); scan.mutate(); }} onRemove={deleteLibrary} /> : null}
      {editOpen ? <div className="lux-library-dialog-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) setEditOpen(false); }} onKeyDown={(event) => { if (event.key === "Escape") setEditOpen(false); }}>
        <div className="lux-library-dialog" role="dialog" aria-modal="true" aria-labelledby={`edit-library-title-${library.id}`}>
          <div className="lux-library-dialog-header"><div><span className="lux-eyebrow">MEDIA LIBRARY</span><h2 id={`edit-library-title-${library.id}`}>{library.name}</h2></div><button ref={dialogCloseRef} className="lux-library-dialog-close" type="button" aria-label={`关闭 ${library.name} 编辑弹窗`} onClick={() => setEditOpen(false)}><X size={20} /></button></div>
          <div className="lux-library-dialog-scroll">
            <p className="lux-library-warning">如果更改了元数据或媒体图片下载的设置，只适用于之后添加到媒体库的新内容。要将更改应用于现有项目，您需要手动刷新其元数据。</p>
            <section className="lux-library-dialog-section"><div className="lux-library-dialog-section-heading"><h3>文件夹</h3><button className="lux-library-toolbar-button" type="button" onClick={() => document.getElementById(`library-root-path-${library.id}`)?.focus()}><Plus size={16} /> 添加</button></div>{library.roots.length === 0 ? <p className="lux-admin-muted">尚未配置根路径。</p> : <div className="lux-library-dialog-root-list">{library.roots.map((root) => <div className="lux-library-dialog-root-row" key={root.id}><span title={root.displayPath}>{root.displayPath}</span><button className="lux-library-dialog-icon" type="button" aria-label={`编辑路径 ${root.displayPath}`} disabled><Pencil size={17} /></button><button className="lux-library-dialog-icon" type="button" aria-label={`删除路径 ${root.displayPath}`} onClick={() => removeRoot.mutate(root.id)} disabled={removeRoot.isPending}><MinusCircle size={18} /></button></div>)}</div>}<form className="lux-library-root-form" onSubmit={(event) => { event.preventDefault(); if (rootPath.trim()) addRoot.mutate(); }}><input id={`library-root-path-${library.id}`} value={rootPath} onChange={(event) => setRootPath(event.target.value)} placeholder="输入 Docker 内的媒体路径" aria-label={`${library.name} 新根路径`} /><button className="lux-library-toolbar-button" type="submit" disabled={addRoot.isPending}><FolderPlus size={15} /> 添加路径</button></form>{rootError ? <p className="lux-error-copy">{rootError}</p> : null}</section>
            <form className="lux-library-dialog-section lux-library-settings-form" onSubmit={saveDetails}><h3>媒体库设置</h3><label htmlFor={`library-name-${library.id}`}>媒体库名称<input id={`library-name-${library.id}`} aria-label={`${library.name} 媒体库名称`} value={editName} onChange={(event) => setEditName(event.target.value)} /></label><label htmlFor={`library-kind-${library.id}`}>媒体库类型<select id={`library-kind-${library.id}`} aria-label={`${library.name} 媒体库类型`} value={editKind} onChange={(event) => setEditKind(event.target.value)}><option value="MOVIE">电影</option><option value="SERIES">剧集</option><option value="MIXED">混合</option></select></label><label className="lux-admin-toggle"><input type="checkbox" checked={library.isEnabled} onChange={(event) => update.mutate({ values: { isEnabled: event.target.checked } })} /><span>启用媒体库</span></label><label className="lux-admin-toggle"><input type="checkbox" checked={library.realtimeWatchEnabled} onChange={(event) => update.mutate({ values: { realtimeWatchEnabled: event.target.checked } })} /><span>实时监听文件变化</span></label><ScraperSelect id={`library-scraper-${library.id}`} value={scraperId} plugins={plugins} onChange={updateScraper} /><ScheduleField label="增量扫描" value={library.incrementalSchedule} onSave={(value) => updateSchedule("incrementalSchedule", value)} /><ScheduleField label="全量校验" value={library.reconciliationSchedule} onSave={(value) => updateSchedule("reconciliationSchedule", value)} /><ScheduleField label="元数据任务" value={library.metadataSchedule} onSave={(value) => updateSchedule("metadataSchedule", value)} />{editError ? <p className="lux-error-copy">{editError}</p> : null}<div className="lux-library-dialog-actions"><button className="lux-library-toolbar-button is-primary" type="submit" disabled={update.isPending}><Save size={15} /> 保存修改</button><button className="lux-library-toolbar-button" type="button" onClick={() => setEditOpen(false)}>取消</button></div></form>
            <section className="lux-library-dialog-section"><div className="lux-library-dialog-section-heading"><h3>媒体库图像</h3><span>JPEG、PNG、WebP，最大 5 MiB</span></div><div className="lux-library-cover-editor"><div className="lux-library-cover-preview">{library.coverImageUrl ? <img src={library.coverImageUrl} alt="" /> : <Image size={24} aria-hidden="true" />}</div><input ref={coverInputRef} className="lux-visually-hidden" type="file" accept="image/jpeg,image/png,image/webp" aria-label={`${library.name} 封面图片`} onChange={selectCover} /><button className="lux-library-toolbar-button" type="button" onClick={() => coverInputRef.current?.click()} disabled={uploadCover.isPending}><Upload size={15} /> {uploadCover.isPending ? "上传中…" : library.coverImageUrl ? "替换封面" : "上传封面"}</button>{coverError ? <p className="lux-error-copy">{coverError}</p> : null}</div></section>
          </div>
        </div>
      </div> : null}
    </article>
  );
}

function LibraryActionMenu({ library, onEdit, onScan, onRemove }: { library: AdminLibrary; onEdit: () => void; onScan: () => void; onRemove: () => void }) {
  const actions: Array<{ label: string; icon: LucideIcon; onClick?: () => void; disabled?: boolean; title?: string }> = [
    { label: "添加到“合集”", icon: ListPlus, disabled: true, title: "合集功能将在后续版本提供" },
    { label: "更改内容类型", icon: Folder, onClick: onEdit },
    { label: "编辑", icon: Pencil, onClick: onEdit },
    { label: "编辑图像", icon: Image, onClick: onEdit },
    { label: "刷新元数据", icon: RefreshCw, disabled: true, title: "请在元数据纠错页面执行" },
    { label: "扫描媒体库文件", icon: RefreshCw, onClick: onScan },
    { label: "移除", icon: MinusCircle, onClick: onRemove },
    { label: "重命名", icon: Pencil, onClick: onEdit },
    { label: "复制", icon: Copy, disabled: true, title: "复制媒体库功能暂未启用" },
  ];
  return <div className="lux-library-action-menu" role="menu" aria-label={`${library.name} 操作`}><div className="lux-library-action-menu-heading">{library.coverImageUrl ? <img src={library.coverImageUrl} alt="" /> : <span><Database size={16} /></span>}<strong>{library.name}</strong></div>{actions.map(({ label, icon: Icon, onClick, disabled, title }, index) => <span key={label}>{index === 6 ? <i className="lux-library-action-divider" aria-hidden="true" /> : null}<button type="button" role="menuitem" onClick={onClick} disabled={disabled} title={title}><Icon size={18} /><span>{label}</span></button></span>)}</div>;
}

function ScraperSelect({ id, value, plugins, onChange }: { id: string; value: string; plugins: AdminPlugin[]; onChange: (value: string) => void }) {
  return <label className="lux-admin-scraper-field" htmlFor={id}>刮削器<select id={id} value={value} onChange={(event) => onChange(event.target.value)}><option value="">仅使用本地元数据</option>{plugins.map((plugin) => <option key={plugin.id} value={plugin.id} disabled={!plugin.available}>{plugin.name}{plugin.available ? "" : plugin.installed ? "（暂不可用）" : "（请先安装）"}</option>)}</select></label>;
}

function ScheduleField({ label, value, onSave }: { label: string; value?: string | null; onSave: (value: string) => void }) {
  const [draft, setDraft] = useState(value ?? "");
  return <label className="lux-admin-schedule-field"><span>{label}</span><div><input value={draft} onChange={(event) => setDraft(event.target.value)} placeholder="interval:1h（留空关闭）" /><button className="lux-library-dialog-icon" type="button" aria-label={`保存${label}`} onClick={() => onSave(draft)}><Save size={14} /></button></div></label>;
}

function libraryKindLabel(kind: string) {
  if (kind === "MOVIE") return "影片";
  if (kind === "SERIES") return "电视剧";
  return "混合内容";
}

function AdminLibraryState({ label, error = false }: { label: string; error?: boolean }) {
  return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">LUX ADMIN</span><h1>{error ? "媒体库加载失败" : "正在加载媒体库"}</h1><p>{label}</p></section>;
}
