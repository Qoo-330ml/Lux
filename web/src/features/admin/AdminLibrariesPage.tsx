import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Database, FolderPlus, Image, Pencil, Play, Plus, Save, Trash2, Upload, X } from "lucide-react";
import { ChangeEvent, FormEvent, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminLibrary, AdminPlugin } from "../../lib/api/types";
import { formatAdminDate } from "./date";

export function AdminLibrariesPage() {
  const queryClient = useQueryClient();
  const libraries = useQuery({ queryKey: queryKeys.adminLibraries, queryFn: () => api.adminLibraries() });
  const plugins = useQuery({ queryKey: queryKeys.adminPlugins, queryFn: () => api.adminPlugins() });
  const [name, setName] = useState("");
  const [kind, setKind] = useState("MOVIE");
  const [watchEnabled, setWatchEnabled] = useState(true);
  const [scraperId, setScraperId] = useState("");
  const [formError, setFormError] = useState("");
  const create = useMutation({
    mutationFn: () => api.createAdminLibrary({ name: name.trim(), kind, realtimeWatchEnabled: watchEnabled, scraperId: scraperId || null }),
    onSuccess: () => { setName(""); setScraperId(""); setFormError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); },
    onError: (error) => setFormError(error.message),
  });

  if (libraries.isPending || plugins.isPending) return <AdminLibraryState label="正在读取媒体库与插件…" />;
  if (libraries.error || plugins.error) return <AdminLibraryState label={libraries.error?.message || plugins.error?.message || "管理数据加载失败"} error />;

  const items = libraries.data.libraries ?? [];
  const pluginItems = plugins.data.plugins ?? [];
  function submit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) { setFormError("请输入媒体库名称"); return; }
    create.mutate();
  }

  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading"><div><span className="lux-eyebrow">LIBRARY MANAGEMENT</span><h1>媒体库</h1><p>管理媒体根路径、扫描计划和实时监听。</p></div></header>
      <section className="lux-admin-panel lux-admin-create-panel">
        <div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">NEW LIBRARY</span><h2>添加媒体库</h2></div><Plus size={20} className="lux-admin-panel-icon" /></div>
        <form className="lux-admin-form lux-admin-create-form" onSubmit={submit}>
          <label>名称<input value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：电影" /></label>
          <label>类型<select value={kind} onChange={(event) => setKind(event.target.value)}><option value="MOVIE">电影</option><option value="SERIES">剧集</option><option value="MIXED">混合</option></select></label>
          <ScraperSelect value={scraperId} plugins={pluginItems} onChange={setScraperId} />
          <label className="lux-admin-toggle"><input type="checkbox" checked={watchEnabled} onChange={(event) => setWatchEnabled(event.target.checked)} /><span>启用实时监听</span></label>
          <button className="lux-button lux-button-primary" type="submit" disabled={create.isPending}><Plus size={16} /> {create.isPending ? "创建中…" : "创建媒体库"}</button>
        </form>
        {formError ? <p className="lux-error-copy">{formError}</p> : null}
      </section>
      <div className="lux-admin-library-list">
        {items.length === 0 ? <div className="lux-admin-empty"><Database size={24} /><h2>还没有媒体库</h2><p>创建第一个媒体库后，Lux 才能开始索引内容。</p></div> : items.map((library) => <LibraryAdminCard key={library.id} library={library} plugins={pluginItems} />)}
      </div>
    </div>
  );
}

function LibraryAdminCard({ library, plugins }: { library: AdminLibrary; plugins: AdminPlugin[] }) {
  const queryClient = useQueryClient();
  const [rootPath, setRootPath] = useState("");
  const [rootError, setRootError] = useState("");
  const [scraperId, setScraperId] = useState(library.scraperId ?? "");
  const [editOpen, setEditOpen] = useState(false);
  const [editName, setEditName] = useState(library.name);
  const [editKind, setEditKind] = useState(library.kind);
  const [editError, setEditError] = useState("");
  const [coverError, setCoverError] = useState("");
  const coverInputRef = useRef<HTMLInputElement>(null);
  const update = useMutation({ mutationFn: (input: Record<string, unknown>) => api.updateAdminLibrary(library.id, input), onSuccess: () => { setEditOpen(false); setEditError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); } });
  const uploadCover = useMutation({ mutationFn: (file: File) => api.updateAdminLibraryCover(library.id, file), onSuccess: () => { setCoverError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); }, onError: (error) => setCoverError(error.message) });
  const addRoot = useMutation({ mutationFn: () => api.addAdminLibraryRoot(library.id, rootPath.trim()), onSuccess: () => { setRootPath(""); setRootError(""); void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }); }, onError: (error) => setRootError(error.message) });
  const removeRoot = useMutation({ mutationFn: (rootId: string) => api.deleteAdminLibraryRoot(library.id, rootId), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }) });
  const scan = useMutation({ mutationFn: () => api.startAdminScan(library.id), onSuccess: () => { void queryClient.invalidateQueries({ queryKey: queryKeys.adminHealth }); void queryClient.invalidateQueries({ queryKey: queryKeys.adminJobs() }); } });
  const remove = useMutation({ mutationFn: () => api.deleteAdminLibrary(library.id), onSuccess: () => void queryClient.invalidateQueries({ queryKey: queryKeys.adminLibraries }) });
  const updateSchedule = (key: string, value: string) => update.mutate({ [key]: value || null });
  const updateScraper = (value: string) => { setScraperId(value); update.mutate({ scraperId: value || null }); };
  const saveDetails = (event: FormEvent) => {
    event.preventDefault();
    const nextName = editName.trim();
    if (!nextName) {
      setEditError("请输入媒体库名称");
      return;
    }
    setEditError("");
    update.mutate({ name: nextName, kind: editKind });
  };
  const selectCover = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) uploadCover.mutate(file);
  };

  return (
    <article className="lux-admin-library-card">
      <div className="lux-admin-library-heading"><div><span className="lux-eyebrow">{library.kind}</span><h2>{library.name}</h2><p>{library.roots.length} 个根路径 · {library.lastScanAt ? `上次扫描 ${formatAdminDate(library.lastScanAt)}` : "尚未扫描"}</p></div><div className="lux-admin-card-actions"><button className="lux-button lux-button-secondary" type="button" onClick={() => { setEditOpen((open) => !open); setEditName(library.name); setEditKind(library.kind); }}><Pencil size={15} /> {editOpen ? "收起编辑" : "编辑"}</button><button className="lux-button lux-button-secondary" type="button" onClick={() => scan.mutate()} disabled={scan.isPending}><Play size={15} /> {scan.isPending ? "启动中…" : "立即扫描"}</button><button className="lux-icon-button lux-danger-icon" type="button" aria-label={`删除 ${library.name}`} onClick={() => { if (window.confirm(`确定删除媒体库“${library.name}”？`)) remove.mutate(); }} disabled={remove.isPending}><Trash2 size={17} /></button></div></div>
      {editOpen ? <div className="lux-admin-library-edit">
        <form className="lux-admin-form lux-admin-library-details-form" onSubmit={saveDetails}>
          <label>媒体库名称<input value={editName} onChange={(event) => setEditName(event.target.value)} aria-label={`${library.name} 媒体库名称`} /></label>
          <label>媒体库类型<select value={editKind} onChange={(event) => setEditKind(event.target.value)} aria-label={`${library.name} 媒体库类型`}><option value="MOVIE">电影</option><option value="SERIES">剧集</option><option value="MIXED">混合</option></select></label>
          <div className="lux-admin-edit-actions"><button className="lux-button lux-button-primary" type="submit" disabled={update.isPending}><Save size={15} /> 保存修改</button><button className="lux-button lux-button-secondary" type="button" onClick={() => setEditOpen(false)}><X size={15} /> 取消</button></div>
        </form>
        {editError ? <p className="lux-error-copy">{editError}</p> : null}
        <div className="lux-admin-cover-editor">
          <div className="lux-admin-cover-preview">{library.coverImageUrl ? <img src={library.coverImageUrl} alt={`${library.name} 封面`} /> : <Image size={26} aria-hidden="true" />}</div>
          <div><strong>媒体库封面</strong><p>支持 JPEG、PNG、WebP，最大 5 MiB。</p><input ref={coverInputRef} className="lux-visually-hidden" type="file" accept="image/jpeg,image/png,image/webp" aria-label={`${library.name} 封面图片`} onChange={selectCover} /><button className="lux-button lux-button-secondary" type="button" onClick={() => coverInputRef.current?.click()} disabled={uploadCover.isPending}><Upload size={15} /> {uploadCover.isPending ? "上传中…" : library.coverImageUrl ? "替换封面" : "上传封面"}</button>{coverError ? <p className="lux-error-copy">{coverError}</p> : null}</div>
        </div>
      </div> : null}
      <div className="lux-admin-library-body">
        <div className="lux-admin-subpanel"><div className="lux-admin-subpanel-heading"><strong>根路径</strong><span>{library.roots.length} 个</span></div>{library.roots.length === 0 ? <p className="lux-admin-muted">尚未配置根路径。</p> : <div className="lux-admin-root-list">{library.roots.map((root) => <div className="lux-admin-root-row" key={root.id}><FolderPlus size={16} /><span title={root.displayPath}>{root.displayPath}</span><span className={root.isAvailable && root.isWritable ? "lux-root-state is-ok" : "lux-root-state is-warn"}>{root.isAvailable ? (root.isWritable ? "可读写" : "只读") : "不可用"}</span><button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`删除路径 ${root.displayPath}`} onClick={() => removeRoot.mutate(root.id)} disabled={removeRoot.isPending}><Trash2 size={14} /></button></div>)}</div>}<form className="lux-admin-root-form" onSubmit={(event) => { event.preventDefault(); if (rootPath.trim()) addRoot.mutate(); }}><input value={rootPath} onChange={(event) => setRootPath(event.target.value)} placeholder="输入 Docker 内的媒体路径" aria-label={`${library.name} 新根路径`} /><button className="lux-button lux-button-secondary" type="submit" disabled={addRoot.isPending}><FolderPlus size={15} /> 添加路径</button></form>{rootError ? <p className="lux-error-copy">{rootError}</p> : null}</div>
        <div className="lux-admin-subpanel"><div className="lux-admin-subpanel-heading"><strong>计划与策略</strong><Save size={16} className="lux-admin-panel-icon" /></div><label className="lux-admin-toggle"><input type="checkbox" checked={library.isEnabled} onChange={(event) => update.mutate({ isEnabled: event.target.checked })} /><span>启用媒体库</span></label><label className="lux-admin-toggle"><input type="checkbox" checked={library.realtimeWatchEnabled} onChange={(event) => update.mutate({ realtimeWatchEnabled: event.target.checked })} /><span>实时监听文件变化</span></label><ScraperSelect value={scraperId} plugins={plugins} onChange={updateScraper} /><ScheduleField label="增量扫描" value={library.incrementalSchedule} onSave={(value) => updateSchedule("incrementalSchedule", value)} /><ScheduleField label="全量校验" value={library.reconciliationSchedule} onSave={(value) => updateSchedule("reconciliationSchedule", value)} /><ScheduleField label="元数据任务" value={library.metadataSchedule} onSave={(value) => updateSchedule("metadataSchedule", value)} /></div>
      </div>
    </article>
  );
}

function ScraperSelect({ value, plugins, onChange }: { value: string; plugins: AdminPlugin[]; onChange: (value: string) => void }) {
  return <label className="lux-admin-scraper-field">刮削器<select value={value} onChange={(event) => onChange(event.target.value)}><option value="">仅使用本地元数据</option>{plugins.map((plugin) => <option key={plugin.id} value={plugin.id} disabled={!plugin.available}>{plugin.name}{plugin.available ? "" : plugin.installed ? "（请先配置）" : "（请先安装）"}</option>)}</select></label>;
}

function ScheduleField({ label, value, onSave }: { label: string; value?: string | null; onSave: (value: string) => void }) {
  const [draft, setDraft] = useState(value ?? "");
  return <label className="lux-admin-schedule-field"><span>{label}</span><div><input value={draft} onChange={(event) => setDraft(event.target.value)} placeholder="interval:1h（留空关闭）" /><button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`保存${label}`} onClick={() => onSave(draft)}><Save size={14} /></button></div></label>;
}

function AdminLibraryState({ label, error = false }: { label: string; error?: boolean }) { return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">LUX ADMIN</span><h1>{error ? "媒体库加载失败" : "正在加载媒体库"}</h1><p>{label}</p></section>; }
