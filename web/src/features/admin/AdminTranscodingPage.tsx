import { useQuery } from "@tanstack/react-query";
import { Cpu, Gauge } from "lucide-react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminTranscodingSession } from "../../lib/api/types";
import {
  formatBytes,
  formatFrameRate,
  formatKbps,
  formatSpeed,
  tierLabel,
} from "../player/player-stats";
import { AdminTranscodingPanel } from "./AdminTranscodingPanel";

/**
 * Dedicated transcoding settings group: policy, hardware self-test and live progress.
 *
 * The settings panel is reused from the server-settings page, but everything transcoding-related
 * now lives here so the general settings page stays about access, playback defaults and network.
 */
export function AdminTranscodingPage() {
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

  if (settings.isPending) {
    return <TranscodingPageState label="正在读取转码设置…" />;
  }
  if (settings.error) {
    return <TranscodingPageState label={settings.error.message} error />;
  }

  const summary = sessions.data?.summary;
  const list = sessions.data?.sessions ?? [];

  return (
    <div className="lux-admin-page">
      <header className="lux-admin-page-heading">
        <div>
          <h1>转码</h1>
          <p>服务端转码策略、硬件编码器自检与实时转码进度。</p>
        </div>
      </header>

      <AdminTranscodingPanel settings={settings.data} />

      <section className="lux-admin-panel" aria-labelledby="transcoding-progress-heading">
        <div className="lux-admin-panel-heading">
          <div>
            <h2 id="transcoding-progress-heading">实时转码进度</h2>
            <p>每 2 秒刷新；只显示当前活动的服务端 HLS 会话，停止或过期后自动消失。</p>
          </div>
          <Gauge size={20} className="lux-admin-panel-icon" />
        </div>

        <div className="lux-admin-transcoding-summary" aria-label="转码会话汇总">
          <SummaryChip label="活动会话" value={summary ? String(summary.activeCount) : "—"} />
          <SummaryChip label="硬件编码" value={summary ? String(summary.hardwareCount) : "—"} />
          <SummaryChip label="软件编码" value={summary ? String(summary.softwareCount) : "—"} />
          <SummaryChip label="封装/音频转码" value={summary ? String(summary.streamCopyCount) : "—"} />
          <SummaryChip label="已输出" value={summary ? formatBytes(summary.outputBytes) ?? "0 B" : "—"} />
        </div>

        {sessions.isPending ? (
          <p className="lux-admin-field-hint">正在读取转码会话…</p>
        ) : sessions.error ? (
          <p className="lux-error-copy">{sessions.error.message}</p>
        ) : list.length === 0 ? (
          <div className="lux-admin-dashboard-empty" role="status">
            <Cpu size={22} />
            <div><strong>当前没有正在进行的转码</strong><span>用户开始服务端转码播放后，会在这里看到实时帧率、速度与输出码率。</span></div>
          </div>
        ) : (
          <div className="lux-transcoding-sessions" aria-label="转码会话">
            {list.map((session) => <TranscodingSessionRow key={session.sessionId} session={session} />)}
          </div>
        )}
      </section>
    </div>
  );
}

function TranscodingSessionRow({ session }: { session: AdminTranscodingSession }) {
  const progress = session.progress;
  const bitrate = progress?.latestSegmentBitrateKbps ?? progress?.bitrateKbps;
  return (
    <article className="lux-transcoding-session">
      <header className="lux-transcoding-session-head">
        <div className="lux-transcoding-session-title">
          <strong>{session.title ?? "未知条目"}</strong>
          <small>
            {[session.userName ?? "未知用户", tierLabel(session.tier), session.container?.toUpperCase()]
              .filter(Boolean)
              .join(" · ")}
          </small>
        </div>
        <span className={`lux-transcoding-session-state${progress?.finished ? " is-finished" : ""}`}>
          {progress ? (progress.finished ? "编码结束" : "编码中") : "等待 FFmpeg 进度"}
        </span>
      </header>
      <dl className="lux-transcoding-session-metrics">
        <Metric label="编码帧率" value={formatFrameRate(progress?.fps)} />
        <Metric label="编码速度" value={formatSpeed(progress?.speed)} />
        <Metric label="输出码率" value={formatKbps(bitrate)} />
        <Metric label="平均码率" value={formatKbps(progress?.averageBitrateKbps)} />
        <Metric label="已输出" value={formatBytes(progress?.directoryBytes)} />
        <Metric label="编码位置" value={formatOutTime(progress?.outTimeSeconds)} />
        <Metric label="丢帧" value={progress ? String(progress.droppedFrames ?? 0) : null} />
        <Metric label="视频/音频" value={[session.videoCodec?.toUpperCase(), session.audioCodec?.toUpperCase()].filter(Boolean).join(" / ") || null} />
      </dl>
    </article>
  );
}

function Metric({ label, value }: { label: string; value: string | null }) {
  return (
    <div className="lux-transcoding-metric">
      <dt>{label}</dt>
      <dd>{value ?? "—"}</dd>
    </div>
  );
}

function SummaryChip({ label, value }: { label: string; value: string }) {
  return (
    <div className="lux-transcoding-summary-chip">
      <small>{label}</small>
      <strong>{value}</strong>
    </div>
  );
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
