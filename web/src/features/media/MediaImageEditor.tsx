import { Image as ImageIcon, LoaderCircle, Search, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { LuxSelect } from "../../components/LuxSelect";
import { api } from "../../lib/api/client";
import type { ImageSearchResult, ItemImage, MediaItem } from "../../lib/api/types";
import { mediaTitle } from "../home/media";

type MediaImageEditorProps = {
  item: MediaItem;
  onClose: () => void;
};

const imageTypes = [
  { value: "POSTER", label: "海报" },
  { value: "LOGO", label: "徽标" },
  { value: "THUMB", label: "缩略图" },
  { value: "BANNER", label: "横幅图" },
  { value: "DISC", label: "光盘封面" },
  { value: "ART", label: "艺术图" },
  { value: "WALLPAPER", label: "壁纸" },
] as const;

const languageOptions = [
  { value: "zh-CN", label: "简体中文" },
  { value: "en-US", label: "English" },
  { value: "", label: "不限语言" },
];

const sourceOptions = [
  { value: "TMDB", label: "TMDb" },
  { value: "", label: "不限来源" },
];

export function MediaImageEditor({ item, onClose }: MediaImageEditorProps) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const [imageType, setImageType] = useState("POSTER");
  const [language, setLanguage] = useState("zh-CN");
  const [source, setSource] = useState("TMDB");
  const [images, setImages] = useState<ItemImage[]>([]);
  const [results, setResults] = useState<ImageSearchResult[]>([]);
  const [loading, setLoading] = useState(true);
  const [searching, setSearching] = useState(false);
  const [selecting, setSelecting] = useState<string>();
  const [error, setError] = useState<string>();

  useEffect(() => {
    closeRef.current?.focus();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", closeOnEscape);
    void api.itemImages(item.id)
      .then((response) => setImages(response.images ?? []))
      .catch(() => setError("现有图片读取失败。"))
      .finally(() => setLoading(false));
    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      document.body.style.overflow = previousOverflow;
    };
  }, [item.id, onClose]);

  async function search() {
    setSearching(true);
    setError(undefined);
    try {
      const response = await api.searchItemImages(item.id, { imageType, language, source });
      setResults(response.images ?? []);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "图片搜索失败，请重试。");
    } finally {
      setSearching(false);
    }
  }

  async function select(result: ImageSearchResult) {
    setSelecting(result.id);
    setError(undefined);
    try {
      const response = await api.selectItemImage(item.id, { imageType, url: result.url, language: result.language });
      setImages((current) => [
        ...current.filter((image) => image.imageType !== imageType || image.imageIndex !== 0),
        response.image,
      ]);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "图片保存失败，请重试。");
    } finally {
      setSelecting(undefined);
    }
  }

  const currentImage = images.find((image) => image.imageType === imageType && image.imageIndex === 0);
  const selectedLabel = imageTypes.find((entry) => entry.value === imageType)?.label ?? "图片";

  return (
    <div className="lux-media-editor-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <section className="lux-media-editor lux-image-editor" role="dialog" aria-modal="true" aria-labelledby="lux-image-editor-title">
        <header className="lux-media-editor-header">
          <div>
            <span className="lux-eyebrow">EDIT IMAGES</span>
            <h2 id="lux-image-editor-title">编辑图像</h2>
            <p>{mediaTitle(item)} · {selectedLabel}</p>
          </div>
          <button ref={closeRef} className="lux-media-editor-close" type="button" aria-label="关闭图像编辑" onClick={onClose}><X size={18} /></button>
        </header>
        <div className="lux-image-editor-body">
          <div className="lux-image-type-tabs" role="tablist" aria-label="图像类型">
            {imageTypes.map((entry) => (
              <button key={entry.value} className={entry.value === imageType ? "is-active" : ""} type="button" role="tab" aria-selected={entry.value === imageType} onClick={() => { setImageType(entry.value); setResults([]); }}>
                <ImageIcon size={15} /> {entry.label}
              </button>
            ))}
          </div>
          <div className="lux-image-editor-toolbar">
            <label><span>语言</span><LuxSelect value={language} options={languageOptions} onChange={setLanguage} aria-label="图片语言" /></label>
            <label><span>来源</span><LuxSelect value={source} options={sourceOptions} onChange={setSource} aria-label="图片来源" /></label>
            <button className="lux-button lux-button-primary" type="button" disabled={searching} onClick={() => void search}><Search size={16} /> {searching ? "搜索中…" : "搜索"}</button>
          </div>
          {error ? <p className="lux-editor-error" role="alert">{error}</p> : null}
          <div className="lux-image-editor-current">
            <span className="lux-editor-section-label">当前{selectedLabel}</span>
            <div className="lux-image-current-card">
              {currentImage ? <img src={currentImage.url} alt={`${selectedLabel} 当前图片`} /> : <div><ImageIcon size={26} /><span>尚未设置</span></div>}
            </div>
          </div>
          <div className="lux-image-results" aria-busy={searching}>
            <span className="lux-editor-section-label">搜索结果</span>
            {searching ? <div className="lux-image-result-state"><LoaderCircle className="lux-spin" size={21} /> 正在搜索图片…</div> : null}
            {!searching && !results.length ? <div className="lux-image-result-state">选择语言和来源后搜索{selectedLabel}。</div> : null}
            <div className="lux-image-result-grid">
              {results.map((result) => (
                <button className="lux-image-result" type="button" key={result.id} disabled={Boolean(selecting)} onClick={() => void select(result)}>
                  <img src={result.url} alt={`${selectedLabel} 搜索结果`} />
                  <span>{selecting === result.id ? "保存中…" : `${result.source}${result.language ? ` · ${result.language}` : ""}`}</span>
                </button>
              ))}
            </div>
          </div>
        </div>
        {loading ? <div className="lux-image-editor-loading">正在读取本地图片…</div> : null}
      </section>
    </div>
  );
}
