import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Clapperboard, Cpu, Square } from "lucide-react";
import { useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  AdminTranscodingHistoryItem,
  AdminTranscodingSession,
  AdminTranscodingSessions,
  AdminTranscodingSettings,
} from "../../lib/api/types";
import {
  formatBytes,
  formatFrameRate,
  formatKbps,
  formatSpeed,
  tierLabel,
} from "../player/player-stats";
import { AdminTranscodingPanel } from "./AdminTranscodingPanel";
import { LuxButton, useLuxConfirm } from "../../components/ui";
import "./transcoding.css";

/**
 * Dedicated transcoding settings group: policy, hardware self-test and live progress.
 *
 * The settings panel is reused from the server-settings page, but everything transcoding-related
 * now lives here so the general settings page stays about access, playback defaults and network.
 */
const HISTORY_PAGE_SIZE = 10;
type TranscodingTab = "live" | "history" | "settings";

export function AdminTranscodingPage() {
  const confirm = useLuxConfirm();
  const queryClient = useQueryClient();
  const [historyPage, setHistoryPage] = useState(1);
  const [tab, setTab] = useState<TranscodingTab>("live");
  const settings = useQuery({
    queryKey: queryKeys.adminSettings,
    queryFn: () => api.adminSettings(),
  });
  const sessions = useQuery({
    queryKey: queryKeys.adminTranscodingSessions,
    queryFn: ({ signal }) => api.adminTranscodingSessions(signal),
    refetchInterval: 2_000,
    staleTime: 1_000,
    retry: false,
  });

  const history = useQuery({
    queryKey: queryKeys.adminTranscodingHistory(historyPage, HISTORY_PAGE_SIZE),
    queryFn: ({ signal }) => api.adminTranscodingHistory(historyPage, HISTORY_PAGE_SIZE, signal),
    staleTime: 5_000,
    retry: false,
  });
  const stopSession = useMutation({
    mutationFn: (sessionId: string) => api.stopAdminTranscodingSession(sessionId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminTranscodingSessions });
      void queryClient.invalidateQueries({
        queryKey: queryKeys.adminTranscodingHistory(historyPage, HISTORY_PAGE_SIZE),
      });
    },
  });

  if (settings.isPending) {
    return <TranscodingPageState label="正在读取转码设置…" />;
  }
  if (settings.error) {
    return <TranscodingPageState label={settings.error.message} error />;
  }

  const summary = sessions.data?.summary;
  const list = sessions.data?.sessions ?? [];
  const limits = settings.data.transcoding;
  const tabs: Array<{ key: TranscodingTab; label: string; badge?: number }> = [
    { key: "live", label: "实时进度", badge: summary?.activeCount },
    { key: "history", label: "转码历史" },
    { key: "settings", label: "转码设置" },
  ];

  return (
    <div className="lux-admin-page lux-tc-page">
      <header className="lux-admin-page-heading">
        <div>
          <h1>转码</h1>
          <p>服务端转码策略、硬件编码器自检与实时转码进度。</p>
        </div>
      </header>

      <div className="lux-tc-tabs" role="tablist" aria-label="转码页面分区">
        {tabs.map(({ key, label, badge }) => (
          <button key={key} type="button" role="tab" aria-selected={tab === key} className={tab === key ? "is-active" : undefined} onClick={() => setTab(key)}>
            {label}
            {badge ? <em>{badge}</em> : null}
          </button>
        ))}
      </div>

      <div className="lux-tc-panel" role="tabpanel" hidden={tab !== "live"}>
      <section className="lux-tc-card" aria-labelledby="transcoding-progress-heading">
            <header className="lux-tc-card-head">
              <div>
                <h2 id="transcoding-progress-heading">实时转码进度</h2>
                <p>每 2 秒刷新；只显示当前活动的服务端 HLS 会话，停止或过期后自动消失。</p>
              </div>
              <LiveStatus summary={summary} limits={limits} />
            </header>

            {sessions.isPending ? (
              <p className="lux-admin-field-hint">正在读取转码会话…</p>
            ) : sessions.error ? (
              <p className="lux-error-copy">{sessions.error.message}</p>
            ) : list.length === 0 ? (
              <div className="lux-tc-empty" role="status">
                <Cpu size={18} />
                <div>
                  <strong>当前没有正在进行的转码</strong>
                  <span>用户开始服务端转码播放后，会在这里看到实时帧率、速度与输出码率。</span>
                </div>
              </div>
            ) : (
              <div className="lux-tc-sessions" aria-label="转码会话">
                {list.map((session) => (
                  <TranscodingSessionRow
                    key={session.sessionId}
                    session={session}
                    stopping={stopSession.isPending && stopSession.variables === session.sessionId}
                    onStop={async () => {
                      const title = session.title ?? "该条目";
                      if (!(await confirm(`停止「${title}」的转码会话？播放会立即中断。`))) return;
                      stopSession.mutate(session.sessionId);
                    }}
                  />
                ))}
              </div>
            )}
            {stopSession.error ? <p className="lux-error-copy">{stopSession.error.message}</p> : null}
          </section>

      </div>

      <div className="lux-tc-panel" role="tabpanel" hidden={tab !== "settings"}>
        <AdminTranscodingPanel settings={settings.data} />
      </div>

      <div className="lux-tc-panel" role="tabpanel" hidden={tab !== "history"}>
          <section className="lux-tc-card" aria-labelledby="transcoding-history-heading">
            <header className="lux-tc-card-head">
              <div>
                <h2 id="transcoding-history-heading">转码历史</h2>
                <p>最近完成、失败或过期的服务端转码；平均速度按编码时长与墙钟时长计算。</p>
              </div>
            </header>
            {history.isPending ? (
              <p className="lux-admin-field-hint">正在读取转码历史…</p>
            ) : history.error ? (
              <p className="lux-error-copy">{history.error.message}</p>
            ) : (history.data?.items.length ?? 0) === 0 ? (
              <p className="lux-admin-field-hint">还没有转码历史。</p>
            ) : (
              <>
                <div className="lux-tc-history" aria-label="转码历史记录">
                  {(history.data?.items ?? []).map((item) => (
                    <TranscodingHistoryRow key={item.id} item={item} />
                  ))}
                </div>
                <div className="lux-tc-pager">
                  <span>
                    共 {history.data?.total ?? 0} 条 · 第 {history.data?.page ?? historyPage} 页
                  </span>
                  <div>
                    <LuxButton tone="secondary"
                      type="button"
                      disabled={historyPage <= 1}
                      onClick={() => setHistoryPage((page) => Math.max(1, page - 1))}
                    >
                      上一页
                    </LuxButton>
                    <LuxButton tone="secondary"
                      type="button"
                      disabled={(history.data?.total ?? 0) <= historyPage * HISTORY_PAGE_SIZE}
                      onClick={() => setHistoryPage((page) => page + 1)}
                    >
                      下一页
                    </LuxButton>
                  </div>
                </div>
              </>
            )}
          </section>
      </div>
    </div>
  );
}

function TranscodingHistoryRow({ item }: { item: AdminTranscodingHistoryItem }) {
  return (
    <article className={`lux-tc-history-row ${outcomeTone(item.outcome)}`}>
      <OutcomeBadge outcome={item.outcome} />
      <div className="lux-tc-history-main">
        <strong>{item.title ?? "未知条目"}</strong>
        <small>
          {[item.userName ?? "未知用户", tierLabel(item.tier), formatClock(item.wallSeconds), formatDateTime(item.finishedAt)]
            .filter(Boolean)
            .join(" · ")}
        </small>
        {item.failureReason ? (
          <small className="lux-tc-history-reason">失败原因：{item.failureReason}</small>
        ) : null}
      </div>
      <div className="lux-tc-history-metrics">
        <span>平均速度 {formatSpeed(item.averageSpeed) ?? "—"}</span>
        <span>输出 {formatBytes(item.outputBytes) ?? "—"}</span>
      </div>
    </article>
  );
}

function outcomeTone(outcome: string) {
  return outcome === "COMPLETED" ? "is-ok" : outcome === "FAILED" ? "is-error" : "is-muted";
}

function OutcomeBadge({ outcome }: { outcome: string }) {
  const label = outcome === "COMPLETED" ? "完成" : outcome === "FAILED" ? "失败" : outcome === "EXPIRED" ? "过期" : outcome;
  return <span className={`lux-tc-outcome ${outcomeTone(outcome)}`}>{label}</span>;
}

function TranscodingSessionRow({
  session,
  stopping,
  onStop,
}: {
  session: AdminTranscodingSession;
  stopping: boolean;
  onStop: () => void;
}) {
  const progress = session.progress;
  const bitrate = progress?.latestSegmentBitrateKbps ?? progress?.bitrateKbps;
  const finished = progress?.finished === true;
  const item = useQuery({
    queryKey: queryKeys.item(session.itemId),
    queryFn: () => api.item(session.itemId),
    staleTime: 5 * 60_000,
    retry: false,
  }).data;
  const isEpisode = session.itemType === "EPISODE" || item?.itemType === "EPISODE";
  const seriesName = isEpisode ? item?.seriesName?.trim() : undefined;
  const heading = seriesName || session.title || "未知条目";
  const episodeLabel =
    seriesName && typeof item?.indexNumber === "number"
      ? `${typeof item.parentIndexNumber === "number" ? `S${String(item.parentIndexNumber).padStart(2, "0")}` : ""}E${String(item.indexNumber).padStart(2, "0")}`
      : null;
  // Series poster for episodes, the item's own poster for movies; unknown episodes wait for the lookup.
  const posterItemId = isEpisode ? item?.seriesId : session.itemId;
  return (
    <article className="lux-tc-session">
      <SessionPoster itemId={posterItemId} title={heading} />
      <div className="lux-tc-session-body">
      <header className="lux-tc-session-head">
        <div className="lux-tc-session-title">
          <strong>{heading}</strong>
          <small>
            {[
              seriesName ? [episodeLabel, session.title].filter(Boolean).join(" ") : null,
              session.userName ?? "未知用户",
              tierLabel(session.tier),
              session.container?.toUpperCase(),
            ]
              .filter(Boolean)
              .join(" · ")}
          </small>
        </div>
        <div className="lux-tc-session-actions">
          <span className={`lux-tc-session-state${finished ? " is-finished" : progress ? " is-live" : ""}`}>
            <i aria-hidden="true" />
            {progress ? (finished ? "编码结束" : "编码中") : "等待 FFmpeg 进度"}
          </span>
          <LuxButton tone="secondary" className="lux-tc-stop"
            type="button"
            aria-label={`停止 ${session.title ?? "转码会话"}`}
            disabled={stopping}
            onClick={onStop}
          >
            <Square size={13} /> {stopping ? "停止中…" : "停止会话"}
          </LuxButton>
        </div>
      </header>
      <dl className="lux-tc-hero">
        <Metric label="编码帧率" value={formatFrameRate(progress?.fps)} />
        <Metric label="编码速度" value={formatSpeed(progress?.speed)} />
        <Metric label="输出码率" value={formatKbps(bitrate)} />
      </dl>
      <dl className="lux-tc-session-metrics">
        <Metric label="平均码率" value={formatKbps(progress?.averageBitrateKbps)} />
        <Metric label="已输出" value={formatBytes(progress?.directoryBytes)} />
        <Metric label="编码位置" value={formatOutTime(progress?.outTimeSeconds)} />
        <Metric label="丢帧" value={progress ? String(progress.droppedFrames ?? 0) : null} />
        <Metric label="视频/音频" value={[session.videoCodec?.toUpperCase(), session.audioCodec?.toUpperCase()].filter(Boolean).join(" / ") || null} />
      </dl>
      </div>
    </article>
  );
}

function SessionPoster({ itemId, title }: { itemId: string | null | undefined; title: string }) {
  const [failed, setFailed] = useState<string | null>(null);
  const show = itemId && failed !== itemId;
  return (
    <div className="lux-tc-poster">
      {show ? (
        <img
          src={`/api/v1/items/${encodeURIComponent(itemId)}/images/poster`}
          alt={`${title} 海报`}
          loading="lazy"
          onError={() => setFailed(itemId)}
        />
      ) : (
        <Clapperboard size={22} aria-hidden="true" />
      )}
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string | null }) {
  return (
    <div className="lux-tc-metric">
      <dt>{label}</dt>
      <dd>{value ?? "—"}</dd>
    </div>
  );
}

function LiveStatus({
  summary,
  limits,
}: {
  summary: AdminTranscodingSessions["summary"] | undefined;
  limits: AdminTranscodingSettings | undefined;
}) {
  // Only tiers that hit their concurrency cap are worth calling out; the per-tier counts otherwise
  // just repeat the active-session total for the configured encoder.
  const capped = [
    { label: "硬件转码", used: summary?.hardwareCount, limit: limits?.hardwareSessions },
    { label: "软件转码", used: summary?.softwareCount, limit: limits?.softwareSessions },
    { label: "封装/音频转码", used: summary?.streamCopyCount, limit: limits?.remuxSessions },
  ].filter((tier) => tier.used !== undefined && tier.limit !== undefined && tier.limit > 0 && tier.used >= tier.limit);
  return (
    <ul className="lux-tc-status" aria-label="转码会话汇总">
      <li>活动会话 {summary ? summary.activeCount : "—"}</li>
      <li>已输出 {summary ? formatBytes(summary.outputBytes) ?? "0 B" : "—"}</li>
      {capped.map((tier) => (
        <li key={tier.label} className="is-full">{tier.label}已达上限 {tier.used}/{tier.limit}</li>
      ))}
    </ul>
  );
}

function formatClock(seconds: number | null | undefined) {
  return formatOutTime(seconds) ?? "—";
}

function formatDateTime(timestamp: number | null | undefined) {
  if (typeof timestamp !== "number" || !Number.isFinite(timestamp) || timestamp <= 0) return null;
  return new Date(timestamp * 1000).toLocaleString();
}

function formatOutTime(seconds: number | null | undefined) {
  if (typeof seconds !== "number" || !Number.isFinite(seconds) || seconds < 0) return null;
  const total = Math.floor(seconds);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const remainder = total % 60;
  const pad = (value: number) => String(value).padStart(2, "0");
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(remainder)}` : `${pad(minutes)}:${pad(remainder)}`;
}

function TranscodingPageState({ label, error = false }: { label: string; error?: boolean }) {
  return (
    <section className="lux-admin-page-state" role={error ? "alert" : "status"}>
      <h1>{error ? "转码设置加载失败" : "正在加载转码设置"}</h1>
      <p>{label}</p>
    </section>
  );
}
