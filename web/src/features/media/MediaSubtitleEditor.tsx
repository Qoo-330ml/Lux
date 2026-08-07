import { LoaderCircle, Save, Subtitles, X } from "lucide-react";
import { FormEvent, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import type { MediaItem, MediaSource, MediaStream } from "../../lib/api/types";
import { mediaTitle } from "../home/media";
import "./MediaSubtitleEditor.css";

type MediaSubtitleEditorProps = {
  item: MediaItem;
  sourceId?: string;
  onClose: () => void;
  onSaved?: () => void;
};

type SubtitleDraft = {
  title: string;
  language: string;
  isDefault: boolean;
  isForced: boolean;
};

export function MediaSubtitleEditor({ item, sourceId, onClose, onSaved }: MediaSubtitleEditorProps) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const source = useMemo(() => findSource(item.mediaSources, sourceId), [item.mediaSources, sourceId]);
  const subtitles = useMemo(() => source?.streams?.filter(isExternalSubtitle) ?? [], [source]);
  const [selectedIndex, setSelectedIndex] = useState<number>();
  const [draft, setDraft] = useState<SubtitleDraft>();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    closeRef.current?.focus();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      document.body.style.overflow = previousOverflow;
    };
  }, [onClose]);

  useEffect(() => {
    const nextIndex = subtitles.some((stream) => stream.index === selectedIndex)
      ? selectedIndex
      : subtitles[0]?.index;
    setSelectedIndex(nextIndex);
    const stream = subtitles.find((candidate) => candidate.index === nextIndex);
    setDraft(stream ? draftFromStream(stream) : undefined);
  }, [selectedIndex, subtitles]);

  const activeStream = subtitles.find((stream) => stream.index === selectedIndex);

  function updateDraft(field: keyof SubtitleDraft, value: string | boolean) {
    setDraft((current) => current ? { ...current, [field]: value } : current);
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!source || !activeStream || !draft) return;
    setSaving(true);
    setError(undefined);
    try {
      await api.updateItemSubtitle(item.id, activeStream.index, {
        sourceId: source.id,
        title: draft.title.trim() || undefined,
        language: draft.language.trim() || undefined,
        isDefault: draft.isDefault,
        isForced: draft.isForced,
      });
      onSaved?.();
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "字幕属性保存失败，请重试。");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="lux-media-editor-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <section className="lux-media-editor lux-subtitle-editor" role="dialog" aria-modal="true" aria-labelledby="lux-subtitle-editor-title">
        <header className="lux-media-editor-header">
          <div>
            <h2 id="lux-subtitle-editor-title">编辑字幕</h2>
            <p>{mediaTitle(item)} · 仅编辑已索引的外挂字幕属性</p>
          </div>
          <button ref={closeRef} className="lux-media-editor-close" type="button" aria-label="关闭字幕编辑" onClick={onClose}><X size={18} /></button>
        </header>
        <div className="lux-subtitle-editor-body">
          {source ? <p className="lux-muted-copy">当前版本：{sourceLabel(source)}</p> : null}
          {subtitles.length ? (
            <div className="lux-subtitle-list" role="list" aria-label="外挂字幕列表">
              {subtitles.map((stream) => (
                <button
                  className={stream.index === selectedIndex ? "lux-subtitle-option is-active" : "lux-subtitle-option"}
                  data-subtitle-index={stream.index}
                  key={stream.index}
                  type="button"
                  onClick={() => setSelectedIndex(stream.index)}
                >
                  <Subtitles size={16} />
                  <span>{subtitleLabel(stream)}</span>
                  {stream.isDefault ? <small>默认</small> : null}
                  {stream.isForced ? <small>强制</small> : null}
                </button>
              ))}
            </div>
          ) : <div className="lux-image-result-state">当前版本没有可编辑的外挂字幕。</div>}
          {activeStream && draft ? (
            <form id="subtitle-editor-form" className="lux-subtitle-form" onSubmit={save}>
              <label htmlFor="subtitle-title">字幕标题<input id="subtitle-title" value={draft.title} onChange={(event) => updateDraft("title", event.target.value)} /></label>
              <label htmlFor="subtitle-language">语言代码<input id="subtitle-language" value={draft.language} onChange={(event) => updateDraft("language", event.target.value)} placeholder="例如 zho、eng" /></label>
              <div className="lux-subtitle-checks">
                <label><input id="subtitle-default" type="checkbox" checked={draft.isDefault} onChange={(event) => updateDraft("isDefault", event.target.checked)} /> 默认字幕</label>
                <label><input id="subtitle-forced" type="checkbox" checked={draft.isForced} onChange={(event) => updateDraft("isForced", event.target.checked)} /> 强制字幕</label>
              </div>
              <p className="lux-muted-copy">字幕文件内容保持不变，只更新播放时使用的属性。</p>
              {error ? <p className="lux-editor-error" role="alert">{error}</p> : null}
              <footer className="lux-media-editor-footer">
                <span>{saving ? "正在保存…" : "外挂字幕来自媒体文件同目录"}</span>
                <div>
                  <button className="lux-button lux-button-secondary" type="button" onClick={onClose}>取消</button>
                  <button className="lux-button lux-button-primary" type="submit" disabled={saving}><Save size={16} /> {saving ? "保存中…" : "保存"}</button>
                </div>
              </footer>
            </form>
          ) : null}
        </div>
        {saving ? <LoaderCircle className="lux-spin" aria-label="正在保存字幕" /> : null}
      </section>
    </div>
  );
}

function findSource(sources: MediaSource[] | undefined, sourceId?: string) {
  return sources?.find((source) => source.id === sourceId)
    ?? sources?.find((source) => source.isDefault)
    ?? sources?.[0];
}

function isExternalSubtitle(stream: MediaStream) {
  return stream.type?.toUpperCase() === "SUBTITLE" && stream.isExternal === true;
}

function draftFromStream(stream: MediaStream): SubtitleDraft {
  return {
    title: stream.title || "",
    language: stream.language || "",
    isDefault: stream.isDefault === true,
    isForced: stream.isForced === true,
  };
}

function subtitleLabel(stream: MediaStream) {
  return stream.title || stream.language || `字幕轨道 ${stream.index + 1}`;
}

function sourceLabel(source: MediaSource) {
  return source.qualityLabel || source.editionName || source.container?.toUpperCase() || "默认版本";
}
