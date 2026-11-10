import { LoaderCircle, Search, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { api } from "../../lib/api/client";
import type {
  AdminDanmakuMatchJob,
  ManualDanmakuCandidate,
  ManualDanmakuEpisode,
  MediaItem,
} from "../../lib/api/types";
import { mediaTitle } from "../home/media-display";
import { LuxButton, LuxTextField } from "../../components/ui";
import "./MediaDanmakuDialog.css";

type MediaDanmakuDialogProps = {
  item: MediaItem;
  sourceId?: string;
  onClose: () => void;
  onSubmitted?: (job: AdminDanmakuMatchJob) => void;
};

const MATCHABLE_ITEM_TYPES = new Set(["MOVIE", "EPISODE"]);

export function defaultDanmakuQuery(item: MediaItem) {
  if (item.itemType === "EPISODE") {
    return item.seriesName || item.title || item.name || "";
  }
  return item.title || item.name || "";
}

export function MediaDanmakuDialog({ item, sourceId, onClose, onSubmitted }: MediaDanmakuDialogProps) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const [query, setQuery] = useState(() => defaultDanmakuQuery(item));
  const [candidates, setCandidates] = useState<ManualDanmakuCandidate[]>([]);
  const [loading, setLoading] = useState(true);
  const [searching, setSearching] = useState(false);
  const [submitting, setSubmitting] = useState<string>();
  const [overwrite, setOverwrite] = useState(false);
  const [error, setError] = useState<string>();
  const submittingRef = useRef(false);
  submittingRef.current = Boolean(submitting);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const matchable = MATCHABLE_ITEM_TYPES.has(item.itemType ?? "");
  const initialQuery = defaultDanmakuQuery(item);

  const runSearch = useCallback(async (value: string) => {
    const trimmed = value.trim();
    if (!trimmed) {
      setLoading(false);
      setError("请输入影视名称。");
      return;
    }
    setSearching(true);
    setError(undefined);
    try {
      const response = await api.manualDanmakuSearch(item.id, { query: trimmed, sourceId });
      setCandidates(response.items ?? []);
      if (!response.supported) setError("当前弹幕插件不支持手动搜索。");
    } catch (cause) {
      setCandidates([]);
      setError(cause instanceof Error ? cause.message : "弹幕候选搜索失败，请重试。");
    } finally {
      setSearching(false);
      setLoading(false);
    }
  }, [item.id, sourceId]);

  useEffect(() => {
    closeRef.current?.focus();
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const closeOnEscape = (event: KeyboardEvent) => {
      if (!submittingRef.current && event.key === "Escape") onCloseRef.current();
    };
    document.addEventListener("keydown", closeOnEscape);
    if (matchable) {
      void runSearch(initialQuery);
    } else {
      setLoading(false);
      setError("弹幕刮削仅支持电影和分集。");
    }
    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      document.body.style.overflow = previousOverflow;
    };
  }, [item.id, initialQuery, matchable, runSearch]);

  function submitSearch(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    void runSearch(query);
  }

  async function match(candidate: ManualDanmakuCandidate, episode: ManualDanmakuEpisode) {
    const key = `${candidate.animeId}:${episode.episodeId}`;
    setSubmitting(key);
    setError(undefined);
    try {
      const response = await api.startManualDanmakuMatch(item.id, {
        sourceId,
        provider: candidate.provider ?? undefined,
        animeId: candidate.animeId,
        episodeId: episode.episodeId,
        overwrite,
      });
      onSubmitted?.(response.job);
      onClose();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "弹幕匹配任务提交失败，请重试。");
    } finally {
      setSubmitting(undefined);
    }
  }

  return (
    <div className="lux-media-editor-backdrop" role="presentation" onMouseDown={(event) => { if (!submitting && event.target === event.currentTarget) onClose(); }}>
      <section className="lux-media-editor lux-danmaku-dialog" role="dialog" aria-modal="true" aria-labelledby="lux-danmaku-title">
        <header className="lux-media-editor-header">
          <div>
            <h2 id="lux-danmaku-title">刮削弹幕</h2>
            <p>{mediaTitle(item)} · 搜索上游分集并下载 Bilibili XML 弹幕</p>
          </div>
          <button ref={closeRef} className="lux-media-editor-close" type="button" aria-label="关闭弹幕刮削" disabled={Boolean(submitting)} onClick={onClose}><X size={18} /></button>
        </header>
        <form className="lux-danmaku-search" onSubmit={submitSearch}>
          <label htmlFor={`danmaku-query-${item.id}`}>名称<LuxTextField id={`danmaku-query-${item.id}`} value={query} onChange={(event) => setQuery(event.target.value)} disabled={!matchable || searching} /></label>
          <LuxButton tone="primary" data-action="danmaku-search" type="submit" disabled={!matchable || searching}><Search size={16} /> {searching ? "搜索中…" : "搜索"}</LuxButton>
          <label className="lux-danmaku-overwrite">
            <input type="checkbox" checked={overwrite} onChange={(event) => setOverwrite(event.target.checked)} disabled={Boolean(submitting)} />
            <span>覆盖已有弹幕</span>
          </label>
        </form>
        <p className="lux-danmaku-hint">搜索会实时请求上游；过于频繁会被临时限流。</p>
        {error ? <p className="lux-editor-error" role="alert">{error}</p> : null}
        <div className="lux-danmaku-results" aria-busy={loading || searching}>
          <div className="lux-danmaku-results-heading"><span className="lux-editor-section-label">候选结果</span><span>{candidates.length} 个</span></div>
          {loading || searching ? <div className="lux-danmaku-state"><LoaderCircle className="lux-spin" size={20} /> 正在读取候选…</div> : null}
          {!loading && !searching && !candidates.length && !error ? <div className="lux-danmaku-state">没有候选结果，请调整名称后重新搜索。</div> : null}
          <div className="lux-danmaku-candidate-list">
            {candidates.map((candidate) => (
              <section className="lux-danmaku-candidate" key={`${candidate.provider ?? "provider"}:${candidate.animeId}`}>
                <div className="lux-danmaku-candidate-heading">
                  <strong title={candidate.animeTitle}>{candidate.animeTitle}</strong>
                  <small>{candidate.provider || "弹幕源"} · {candidate.episodes.length} 集</small>
                </div>
                {candidate.episodes.length ? (
                  <div className="lux-danmaku-episodes">
                    {candidate.episodes.map((episode) => {
                      const key = `${candidate.animeId}:${episode.episodeId}`;
                      return (
                        <button
                          className="lux-danmaku-episode"
                          data-action="danmaku-match"
                          data-episode-id={episode.episodeId}
                          type="button"
                          key={key}
                          disabled={Boolean(submitting)}
                          aria-label={`匹配 ${candidate.animeTitle} ${episode.title}`}
                          onClick={() => void match(candidate, episode)}
                        >
                          <span>{episode.title || episode.episodeId}</span>
                          {submitting === key ? <LoaderCircle className="lux-spin" size={14} /> : null}
                        </button>
                      );
                    })}
                  </div>
                ) : <small className="lux-danmaku-empty-episodes">没有可选择的弹幕分集</small>}
              </section>
            ))}
          </div>
        </div>
      </section>
    </div>
  );
}
