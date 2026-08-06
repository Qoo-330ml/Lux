import { Check, LoaderCircle, Search, X } from "lucide-react";
import { FormEvent, useEffect, useRef, useState } from "react";
import "./MediaIdentifier.css";
import { api } from "../../lib/api/client";
import type { AdminMetadataCandidate, MediaItem } from "../../lib/api/types";
import { mediaTitle } from "../home/media";

type MediaIdentifierProps = {
  item: MediaItem;
  onClose: () => void;
  onSaved?: () => void;
};

export function MediaIdentifier({ item, onClose, onSaved }: MediaIdentifierProps) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const [query, setQuery] = useState(item.title || item.name || "");
  const [year, setYear] = useState(item.productionYear ? String(item.productionYear) : "");
  const [candidates, setCandidates] = useState<AdminMetadataCandidate[]>([]);
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
    void api.adminItemCandidates(item.id)
      .then((response) => setCandidates(response.items ?? []))
      .catch((cause) => setError(cause instanceof Error ? cause.message : "候选读取失败，请重试。"))
      .finally(() => setLoading(false));
    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      document.body.style.overflow = previousOverflow;
    };
  }, [item.id]);

  async function search(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmedQuery = query.trim();
    if (!trimmedQuery) {
      setError("请输入影视名称。");
      return;
    }
    setSearching(true);
    setError(undefined);
    try {
      const response = await api.searchAdminItemCandidates(
        item.id,
        trimmedQuery,
        year.trim() ? Number(year) : undefined,
      );
      setCandidates(response.items ?? []);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "候选搜索失败，请重试。");
    } finally {
      setSearching(false);
    }
  }

  async function select(candidate: AdminMetadataCandidate, mode: "fillMissing" | "refreshUnlocked") {
    setSelecting(`${candidate.id}:${mode}`);
    setError(undefined);
    try {
      await api.selectAdminMetadata(item.id, candidate.id, mode);
      onSaved?.();
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "元数据匹配结果应用失败，请重试。");
    } finally {
      setSelecting(undefined);
    }
  }

  return (
    <div className="lux-media-editor-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <section className="lux-media-editor lux-identifier-editor" role="dialog" aria-modal="true" aria-labelledby="lux-identifier-title">
        <header className="lux-media-editor-header">
          <div>
            <span className="lux-eyebrow">METADATA MATCH</span>
            <h2 id="lux-identifier-title">匹配元数据</h2>
            <p>{mediaTitle(item)} · 从刮削器候选中选择正确条目</p>
          </div>
          <button ref={closeRef} className="lux-media-editor-close" type="button" aria-label="关闭元数据匹配" onClick={onClose}><X size={18} /></button>
        </header>
        <form className="lux-identifier-search" onSubmit={search}>
          <label htmlFor="identify-query">名称<input id="identify-query" value={query} onChange={(event) => setQuery(event.target.value)} /></label>
          <label htmlFor="identify-year">年份<input id="identify-year" type="number" min="1800" max="2200" value={year} onChange={(event) => setYear(event.target.value)} /></label>
          <button className="lux-button lux-button-primary" data-action="identify-search" type="submit" disabled={searching}><Search size={16} /> {searching ? "搜索中…" : "搜索候选"}</button>
        </form>
        {error ? <p className="lux-editor-error" role="alert">{error}</p> : null}
        <div className="lux-identifier-results" aria-busy={loading || searching}>
          <div className="lux-identifier-results-heading"><span className="lux-editor-section-label">候选结果</span><span>{candidates.length} 个</span></div>
          {loading || searching ? <div className="lux-image-result-state"><LoaderCircle className="lux-spin" size={21} /> 正在读取候选…</div> : null}
          {!loading && !searching && !candidates.length ? <div className="lux-image-result-state">没有候选结果，请调整名称或年份后重新搜索。</div> : null}
          <div className="lux-identifier-list">
            {candidates.map((candidate) => <CandidateCard key={candidate.id} candidate={candidate} selecting={selecting} onSelect={select} />)}
          </div>
        </div>
      </section>
    </div>
  );
}

function CandidateCard({
  candidate,
  selecting,
  onSelect,
}: {
  candidate: AdminMetadataCandidate;
  selecting?: string;
  onSelect: (candidate: AdminMetadataCandidate, mode: "fillMissing" | "refreshUnlocked") => void;
}) {
  const title = valueAsString(candidate.candidate.title) || valueAsString(candidate.candidate.originalTitle) || candidate.providerId;
  const originalTitle = valueAsString(candidate.candidate.originalTitle);
  const overview = valueAsString(candidate.candidate.overview);
  const productionYear = valueAsString(candidate.candidate.productionYear) || valueAsString(candidate.candidate.releaseDate).slice(0, 4);
  return (
    <article className="lux-identifier-card">
      <div className="lux-identifier-card-heading">
        <div><strong>{title}</strong><small>{originalTitle ? `${originalTitle} · ` : ""}{productionYear || "年份未知"} · {candidate.provider} ID {candidate.providerId}</small></div>
        <span className="lux-status-pill">{Math.round(candidate.score)} 分</span>
      </div>
      {overview ? <p>{overview}</p> : null}
      {candidate.fieldDiffs.length ? <div className="lux-identifier-diffs">{candidate.fieldDiffs.map((diff) => <div key={diff.field}><span>{diff.field}</span><small>{displayValue(diff.current)} → {displayValue(diff.candidate)}</small></div>)}</div> : <small className="lux-identifier-no-diff">没有字段差异</small>}
      <div className="lux-identifier-actions">
        <button className="lux-button lux-button-primary" data-action="identify-fill" type="button" disabled={Boolean(selecting)} onClick={() => onSelect(candidate, "fillMissing")}><Check size={15} /> {selecting === `${candidate.id}:fillMissing` ? "应用中…" : "仅补缺"}</button>
        <button className="lux-button lux-button-secondary" data-action="identify-refresh" type="button" title="会替换当前媒体所有未锁定的元数据及图片" disabled={Boolean(selecting)} onClick={() => onSelect(candidate, "refreshUnlocked")}><Search size={15} /> {selecting === `${candidate.id}:refreshUnlocked` ? "应用中…" : "全量替换"}</button>
      </div>
    </article>
  );
}

function valueAsString(value: unknown) {
  return typeof value === "string" || typeof value === "number" ? String(value) : "";
}

function displayValue(value: unknown) {
  return valueAsString(value) || "空";
}
