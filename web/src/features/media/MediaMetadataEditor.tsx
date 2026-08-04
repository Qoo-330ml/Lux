import { Lock, Save, Unlock, X } from "lucide-react";
import { FormEvent, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import type { ItemMetadata, MediaItem, MetadataFieldName } from "../../lib/api/types";
import { mediaTitle } from "../home/media";

type MediaMetadataEditorProps = {
  item: MediaItem;
  onClose: () => void;
  onSaved?: (metadata: ItemMetadata) => void;
};

type MetadataDraft = {
  title: string;
  originalTitle: string;
  overview: string;
  productionYear: string;
};

const fields: Array<{ key: MetadataFieldName; label: string }> = [
  { key: "title", label: "标题" },
  { key: "originalTitle", label: "原标题" },
  { key: "overview", label: "简介" },
  { key: "productionYear", label: "年份" },
];

export function MediaMetadataEditor({ item, onClose, onSaved }: MediaMetadataEditorProps) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const [draft, setDraft] = useState<MetadataDraft>(() => draftFromItem(item));
  const [lockedFields, setLockedFields] = useState<MetadataFieldName[]>([]);
  const [loading, setLoading] = useState(true);
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
    void api.itemMetadata(item.id)
      .then((metadata) => {
        setDraft(draftFromMetadata(metadata));
        setLockedFields(metadata.lockedFields ?? []);
      })
      .catch(() => {
        setError("元数据读取失败，将使用当前详情继续编辑。");
      })
      .finally(() => setLoading(false));
    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      document.body.style.overflow = previousOverflow;
    };
  }, [item.id]);

  function updateField(key: keyof MetadataDraft, value: string) {
    setDraft((current) => ({ ...current, [key]: value }));
  }

  function toggleLock(field: MetadataFieldName) {
    setLockedFields((current) => current.includes(field)
      ? current.filter((value) => value !== field)
      : [...current, field]);
  }

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setSaving(true);
    setError(undefined);
    try {
      const metadata = await api.updateItemMetadata(item.id, {
        title: draft.title,
        originalTitle: draft.originalTitle || undefined,
        overview: draft.overview || undefined,
        productionYear: draft.productionYear ? Number(draft.productionYear) : undefined,
        lockedFields,
      });
      onSaved?.(metadata);
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "元数据保存失败，请重试。");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div
      className="lux-media-editor-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section className="lux-media-editor lux-metadata-editor" role="dialog" aria-modal="true" aria-labelledby="lux-metadata-editor-title">
        <header className="lux-media-editor-header">
          <div>
            <span className="lux-eyebrow">EDIT METADATA</span>
            <h2 id="lux-metadata-editor-title">编辑元数据</h2>
            <p>{mediaTitle(item)}</p>
          </div>
          <button ref={closeRef} className="lux-media-editor-close" type="button" aria-label="关闭元数据编辑" onClick={onClose}><X size={18} /></button>
        </header>
        <form className="lux-metadata-editor-form" onSubmit={save}>
          {fields.map(({ key, label }) => {
            const locked = lockedFields.includes(key);
            const inputId = `metadata-${key}`;
            return (
              <div className={`lux-metadata-field${locked ? " is-locked" : ""}`} key={key}>
                <label htmlFor={inputId}>{label}</label>
                <div className="lux-metadata-field-control">
                  {key === "overview" ? (
                    <textarea id={inputId} value={draft.overview} disabled={locked} onChange={(event) => updateField(key, event.target.value)} rows={6} />
                  ) : (
                    <input id={inputId} type={key === "productionYear" ? "number" : "text"} value={draft[key]} disabled={locked} onChange={(event) => updateField(key, event.target.value)} />
                  )}
                  <button className="lux-metadata-lock" type="button" aria-label={`${locked ? "解锁" : "锁定"}${label}`} aria-pressed={locked} onClick={() => toggleLock(key)}>
                    {locked ? <Lock size={16} /> : <Unlock size={16} />}
                  </button>
                </div>
                <small>{locked ? "已锁定，自动元数据不会覆盖此字段。" : "未锁定"}</small>
              </div>
            );
          })}
          {error ? <p className="lux-editor-error" role="alert">{error}</p> : null}
          <footer className="lux-media-editor-footer">
            <span>{loading ? "正在读取已有元数据…" : "每个字段都可以单独锁定"}</span>
            <div>
              <button className="lux-button lux-button-secondary" type="button" onClick={onClose}>取消</button>
              <button className="lux-button lux-button-primary" type="submit" disabled={saving || loading}><Save size={16} /> {saving ? "保存中…" : "保存"}</button>
            </div>
          </footer>
        </form>
      </section>
    </div>
  );
}

function draftFromItem(item: MediaItem): MetadataDraft {
  return {
    title: item.title || item.name || "",
    originalTitle: "",
    overview: item.overview || "",
    productionYear: item.productionYear ? String(item.productionYear) : "",
  };
}

function draftFromMetadata(metadata: ItemMetadata): MetadataDraft {
  return {
    title: metadata.title,
    originalTitle: metadata.originalTitle || "",
    overview: metadata.overview || "",
    productionYear: metadata.productionYear ? String(metadata.productionYear) : "",
  };
}
