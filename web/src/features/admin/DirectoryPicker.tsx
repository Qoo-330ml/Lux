import { ChevronRight, Folder, LoaderCircle, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type AdminDirectoryEntry } from "../../lib/api/client";

type DirectoryNode = AdminDirectoryEntry & {
  children: string[];
  expanded: boolean;
  loaded: boolean;
  loading: boolean;
  page: number;
  hasMore: boolean;
  error: string;
};

function createNode(entry: AdminDirectoryEntry, expanded = false): DirectoryNode {
  return {
    ...entry,
    children: [],
    expanded,
    loaded: false,
    loading: false,
    page: 0,
    hasMore: false,
    error: "",
  };
}

export function DirectoryPicker({
  initialPath,
  isSubmitting,
  onSelect,
  onClose,
}: {
  initialPath?: string;
  isSubmitting: boolean;
  onSelect: (path: string) => void;
  onClose: () => void;
}) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const [selectedPath, setSelectedPath] = useState(initialPath?.startsWith("/") ? initialPath : "/");
  const [nodes, setNodes] = useState<Record<string, DirectoryNode>>({
    "/": createNode({ name: "/", path: "/" }, true),
  });

  const loadNode = useCallback(async (path: string, page: number, append: boolean) => {
    setNodes((current) => ({
      ...current,
      [path]: { ...current[path], loading: true, error: "" },
    }));
    try {
      const result = await api.adminDirectories(path, page);
      setNodes((current) => {
        const next = { ...current };
        for (const entry of result.directories) {
          next[entry.path] ??= createNode(entry);
        }
        const previousChildren = append ? current[path]?.children ?? [] : [];
        const children = [...new Set([...previousChildren, ...result.directories.map((entry) => entry.path)])];
        next[path] = {
          ...(current[path] ?? createNode({ name: result.path, path: result.path })),
          children,
          expanded: true,
          loaded: true,
          loading: false,
          page: result.page,
          hasMore: result.hasMore,
          error: "",
        };
        return next;
      });
    } catch (error) {
      setNodes((current) => ({
        ...current,
        [path]: {
          ...current[path],
          loading: false,
          loaded: true,
          error: error instanceof Error ? error.message : "目录读取失败",
        },
      }));
    }
  }, []);

  useEffect(() => {
    closeRef.current?.focus();
    void loadNode("/", 1, false);
  }, [loadNode]);

  const toggleNode = (path: string) => {
    const node = nodes[path];
    if (!node || node.loading) return;
    if (node.expanded) {
      setNodes((current) => ({ ...current, [path]: { ...current[path], expanded: false } }));
      return;
    }
    setNodes((current) => ({ ...current, [path]: { ...current[path], expanded: true } }));
    if (!node.loaded) void loadNode(path, 1, false);
  };

  return (
    <section
      className="lux-directory-picker"
      role="region"
      aria-labelledby="lux-directory-picker-title"
      onKeyDown={(event) => { if (event.key === "Escape") onClose(); }}
    >
      <header className="lux-directory-picker-header">
        <div><span className="lux-eyebrow">SERVER FILESYSTEM</span><h4 id="lux-directory-picker-title">选择服务器目录</h4></div>
        <button ref={closeRef} className="lux-library-dialog-icon" type="button" aria-label="关闭目录选择器" onClick={onClose}><X size={17} /></button>
      </header>
      <p className="lux-directory-picker-help">这里显示 Lux 服务可访问的目录；Docker 部署时请选择已经挂载到容器内的媒体目录。</p>
      <div className="lux-directory-picker-selected"><span>已选择</span><strong title={selectedPath}>{selectedPath}</strong></div>
      <div className="lux-directory-tree-scroll" aria-busy={nodes["/"]?.loading}>
        <ul className="lux-directory-tree" role="tree" aria-label="服务器目录树">
          <DirectoryTreeItem
            path="/"
            nodes={nodes}
            selectedPath={selectedPath}
            onSelect={setSelectedPath}
            onToggle={toggleNode}
            onLoadMore={(path, page) => void loadNode(path, page, true)}
          />
        </ul>
      </div>
      <footer className="lux-directory-picker-actions">
        <span>{selectedPath === "/" ? "请选择具体的媒体目录" : isSubmitting ? "正在添加此路径…" : "点击“使用此路径”后将直接添加到媒体库"}</span>
        <button className="lux-library-toolbar-button is-primary" type="button" disabled={selectedPath === "/" || isSubmitting} onClick={() => onSelect(selectedPath)}>{isSubmitting ? "添加中…" : "使用此路径"}</button>
      </footer>
    </section>
  );
}

function DirectoryTreeItem({
  path,
  nodes,
  selectedPath,
  onSelect,
  onToggle,
  onLoadMore,
}: {
  path: string;
  nodes: Record<string, DirectoryNode>;
  selectedPath: string;
  onSelect: (path: string) => void;
  onToggle: (path: string) => void;
  onLoadMore: (path: string, page: number) => void;
}) {
  const node = nodes[path];
  if (!node) return null;
  return <li role="treeitem" aria-expanded={node.expanded} aria-selected={selectedPath === path}>
    <div className={`lux-directory-tree-row${selectedPath === path ? " is-selected" : ""}`}>
      <button className="lux-directory-tree-toggle" type="button" aria-label={`${node.expanded ? "收起" : "展开"}目录 ${node.path}`} aria-expanded={node.expanded} onClick={() => onToggle(path)} disabled={node.loading}>
        {node.loading ? <LoaderCircle className="is-loading" size={15} /> : <ChevronRight size={15} />}
      </button>
      <button className="lux-directory-tree-name" type="button" aria-label={`选择目录 ${node.path}`} onClick={() => onSelect(path)} title={node.path}><Folder size={16} /><span>{node.name}</span></button>
    </div>
    {node.expanded ? <ul role="group">
      {node.children.map((childPath) => <DirectoryTreeItem key={childPath} path={childPath} nodes={nodes} selectedPath={selectedPath} onSelect={onSelect} onToggle={onToggle} onLoadMore={onLoadMore} />)}
      {node.loaded && node.children.length === 0 && !node.error ? <li className="lux-directory-tree-state" role="none">此目录没有子目录</li> : null}
      {node.error ? <li className="lux-directory-tree-state is-error" role="none"><span>{node.error}</span><button type="button" onClick={() => onLoadMore(path, 1)}>重试</button></li> : null}
      {node.hasMore ? <li className="lux-directory-tree-state" role="none"><button type="button" onClick={() => onLoadMore(path, node.page + 1)}>加载更多目录</button></li> : null}
    </ul> : null}
  </li>;
}
