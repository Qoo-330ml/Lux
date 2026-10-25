import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Check, RefreshCw, Save, ServerCog, ShieldAlert, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  AdminSettings,
  AdminTranscodingSettings,
  AdminTranscodingStatus,
} from "../../lib/api/types";

const HARDWARE_ENCODER_LABELS: Record<string, string> = {
  NONE: "不启用（仅软件转码）",
  h264_nvenc: "NVIDIA NVENC",
  h264_qsv: "Intel Quick Sync",
  h264_vaapi: "VAAPI（Linux）",
  h264_videotoolbox: "Apple VideoToolbox",
};

const OUTPUT_HEIGHT_OPTIONS = [
  { value: 0, label: "保持原始分辨率" },
  { value: 360, label: "最高 360p" },
  { value: 480, label: "最高 480p" },
  { value: 720, label: "最高 720p" },
  { value: 1080, label: "最高 1080p" },
  { value: 1440, label: "最高 1440p" },
  { value: 2160, label: "最高 2160p（4K）" },
];

const PRESET_LABELS: Record<string, string> = {
  ultrafast: "ultrafast（最快，画质最低）",
  superfast: "superfast",
  veryfast: "veryfast（默认）",
  faster: "faster",
  fast: "fast",
  medium: "medium（最慢，画质最好）",
};

function numberInput(
  value: number,
  onChange: (value: number) => void,
  bounds: { min: number; max: number },
) {
  return {
    type: "number" as const,
    min: bounds.min,
    max: bounds.max,
    value: String(Number.isFinite(value) ? value : 0),
    onChange: (event: { target: { value: string } }) => {
      const parsed = Number.parseInt(event.target.value, 10);
      onChange(Number.isFinite(parsed) ? parsed : 0);
    },
  };
}

/**
 * Administrator controls for server-side transcoding.
 *
 * Values are applied to sessions started after saving; a running session keeps the parameters it
 * started with. Container-level environment variables still win over the hardware encoder and the
 * global disk budget, which the copy below tells the administrator.
 */
export function AdminTranscodingPanel({ settings }: { settings: AdminSettings }) {
  const queryClient = useQueryClient();
  const current = settings.transcoding;
  const options = settings.transcodingOptions;
  const [draft, setDraft] = useState<AdminTranscodingSettings | undefined>(current);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    setDraft(current);
  }, [current]);

  const [status, setStatus] = useState<AdminTranscodingStatus | null>(
    settings.transcodingStatus ?? null,
  );
  useEffect(() => {
    setStatus(settings.transcodingStatus ?? null);
  }, [settings.transcodingStatus]);

  const save = useMutation({
    mutationFn: (next: AdminTranscodingSettings) => api.updateAdminSettings({ transcoding: next }),
    onSuccess: (data) => {
      setSaved(true);
      setStatus(data.transcodingStatus ?? null);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminSettings });
    },
    onError: () => setSaved(false),
  });
  const probe = useMutation({
    mutationFn: () => api.probeAdminTranscoding(),
    onSuccess: (data) => {
      setStatus(data.transcodingStatus);
      void queryClient.invalidateQueries({ queryKey: queryKeys.adminSettings });
    },
  });

  if (!draft) {
    return (
      <section className="lux-admin-panel lux-admin-settings-panel" aria-labelledby="transcoding-settings-heading">
        <div className="lux-admin-panel-heading">
          <div><h2 id="transcoding-settings-heading">服务端转码</h2></div>
          <ServerCog size={20} className="lux-admin-panel-icon" />
        </div>
        <p className="lux-admin-field-hint">当前服务器版本未返回转码设置。</p>
      </section>
    );
  }

  const encoderValues = ["NONE", ...(options?.hardwareEncoders ?? [])];
  const presetValues = options?.softwarePresets ?? ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium"];
  const edit = (patch: Partial<AdminTranscodingSettings>) => {
    setSaved(false);
    setDraft((previous) => (previous ? { ...previous, ...patch } : previous));
  };

  return (
    <section className="lux-admin-panel lux-admin-settings-panel" aria-labelledby="transcoding-settings-heading">
      <div className="lux-admin-panel-heading">
        <div><h2 id="transcoding-settings-heading">服务端转码</h2></div>
        <ServerCog size={20} className="lux-admin-panel-icon" />
      </div>
      <div className="lux-admin-settings-form">
        <label className="lux-admin-toggle">
          <input
            type="checkbox"
            aria-label="启用服务端转码"
            checked={draft.enabled}
            onChange={(event) => edit({ enabled: event.target.checked })}
          />
          <span>启用服务端转码</span>
        </label>
        <small>只影响实时播放：关闭后本地媒体只做直连播放或浏览器端解码，不再启动播放转码进程。缩略图、媒体信息探测、章节检测等后台任务仍照常使用 FFmpeg/FFprobe；正在播放的会话不受影响。</small>

        <label>
          <span>硬件编码器</span>
          <select
            aria-label="硬件编码器"
            value={draft.hardwareEncoder}
            onChange={(event) => edit({ hardwareEncoder: event.target.value })}
          >
            {encoderValues.map((encoder) => (
              <option key={encoder} value={encoder}>
                {HARDWARE_ENCODER_LABELS[encoder] ?? encoder}
              </option>
            ))}
          </select>
          <small>保存时会自动做一次编码自检；自检不通过的编码器不会用于播放，系统会改用软件转码。部署层 LUX_HLS_HW_ENCODER 优先于此处选择。</small>
        </label>

        <div className="lux-admin-transcoding-status" role="status" aria-label="硬件编码器自检">
          <TranscodingStatusLine status={status} encoder={draft.hardwareEncoder} />
          <button
            type="button"
            className="lux-button lux-button-secondary"
            disabled={probe.isPending || draft.hardwareEncoder === "NONE"}
            onClick={() => probe.mutate()}
          >
            <RefreshCw size={14} className={probe.isPending ? "is-spinning" : undefined} />
            {probe.isPending ? "检测中…" : "重新检测"}
          </button>
        </div>
        {probe.error ? <p className="lux-error-copy">{probe.error.message}</p> : null}

        <label>
          <span>硬件设备路径</span>
          <input
            type="text"
            aria-label="硬件设备路径"
            value={draft.hardwareDevice}
            placeholder="/dev/dri/renderD128"
            onChange={(event) => edit({ hardwareDevice: event.target.value })}
          />
          <small>VAAPI/QSV 使用的 DRM 渲染节点；NVENC 与 VideoToolbox 忽略此值。留空或填写默认值时使用 /dev/dri/renderD128。</small>
        </label>

        <label className="lux-admin-toggle">
          <input
            type="checkbox"
            aria-label="启用硬件解码"
            checked={draft.hardwareDecode}
            onChange={(event) => edit({ hardwareDecode: event.target.checked })}
          />
          <span>启用硬件解码</span>
        </label>
        <small>让 FFmpeg 优先使用 GPU 解码（需要容器映射设备并支持对应源格式）；解码不可用或失败时会自动降级到软件转码。</small>

        <label>
          <span>封装转换 / 音频转码并发上限</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.remuxSessions, (remuxSessions) => edit({ remuxSessions }), { min: 0, max: 16 })}
              aria-label="封装转换并发上限"
            />
            <em>路</em>
          </div>
          <small>0 表示禁用这两个档位；同一上限同时约束封装转换（档位 1）与音频转码（档位 2）。</small>
        </label>

        <label>
          <span>硬件转码并发上限</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.hardwareSessions, (hardwareSessions) => edit({ hardwareSessions }), { min: 0, max: 16 })}
              aria-label="硬件转码并发上限"
            />
            <em>路</em>
          </div>
        </label>

        <label>
          <span>软件转码并发上限</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.softwareSessions, (softwareSessions) => edit({ softwareSessions }), { min: 0, max: 16 })}
              aria-label="软件转码并发上限"
            />
            <em>路</em>
          </div>
          <small>软件转码占用 CPU 较多，0 表示禁用视频软件转码。</small>
        </label>

        <label>
          <span>软件编码 preset</span>
          <select
            aria-label="软件编码 preset"
            value={draft.softwarePreset}
            onChange={(event) => edit({ softwarePreset: event.target.value })}
          >
            {presetValues.map((preset) => (
              <option key={preset} value={preset}>{PRESET_LABELS[preset] ?? preset}</option>
            ))}
          </select>
        </label>

        <label>
          <span>最大输出分辨率</span>
          <select
            aria-label="最大输出分辨率"
            value={String(draft.maxOutputHeight)}
            onChange={(event) => edit({ maxOutputHeight: Number.parseInt(event.target.value, 10) || 0 })}
          >
            {OUTPUT_HEIGHT_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>{option.label}</option>
            ))}
          </select>
          <small>只缩小、不放大，保持宽高比；封装转换与音频转码不改变画面。</small>
        </label>

        <label>
          <span>视频码率上限</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.maxVideoBitrateKbps, (maxVideoBitrateKbps) => edit({ maxVideoBitrateKbps }), { min: 0, max: 100_000 })}
              aria-label="视频码率上限"
            />
            <em>kbps</em>
          </div>
          <small>0 表示使用编码器默认值；不会把客户端请求的更低码率调高。</small>
        </label>

        <label>
          <span>音频码率</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.audioBitrateKbps, (audioBitrateKbps) => edit({ audioBitrateKbps }), { min: 32, max: 512 })}
              aria-label="音频码率"
            />
            <em>kbps</em>
          </div>
        </label>

        <label>
          <span>单会话磁盘配额</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.sessionQuotaMb, (sessionQuotaMb) => edit({ sessionQuotaMb }), { min: 0, max: 65_536 })}
              aria-label="单会话磁盘配额"
            />
            <em>MB</em>
          </div>
          <small>0 表示不限制；超出后新分片请求会被拒绝并回收会话。</small>
        </label>

        <label>
          <span>全局磁盘预算</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.globalBudgetMb, (globalBudgetMb) => edit({ globalBudgetMb }), { min: 0, max: 1_048_576 })}
              aria-label="全局磁盘预算"
            />
            <em>MB</em>
          </div>
          <small>所有 HLS 会话合计上限，0 表示不限制；部署层 LUX_HLS_GLOBAL_DISK_BUDGET_BYTES 优先。</small>
        </label>

        <label>
          <span>最低剩余空间</span>
          <div className="lux-admin-input-with-suffix">
            <input
              {...numberInput(draft.minFreeSpaceMb, (minFreeSpaceMb) => edit({ minFreeSpaceMb }), { min: 0, max: 1_048_576 })}
              aria-label="最低剩余空间"
            />
            <em>MB</em>
          </div>
          <small>低于该值时不再启动新的转码会话；0 表示关闭保护。</small>
        </label>

        <button
          className="lux-button lux-button-primary lux-settings-save"
          type="button"
          disabled={save.isPending}
          onClick={() => save.mutate(draft)}
        >
          <Save size={16} /> {save.isPending ? "保存中…" : "保存转码设置"}
        </button>
      </div>
      {saved ? <p className="lux-settings-saved"><Check size={15} /> 设置已保存，将在新的播放会话生效</p> : null}
      {save.error ? <p className="lux-error-copy">{save.error.message}</p> : null}
    </section>
  );
}

function TranscodingStatusLine({
  status,
  encoder,
}: {
  status: AdminTranscodingStatus | null;
  encoder: string;
}) {
  if (encoder === "NONE") {
    return <span className="lux-admin-transcoding-state">未启用硬件编码，仅使用软件转码。</span>;
  }
  if (!status || status.encoder !== encoder) {
    return <span className="lux-admin-transcoding-state">尚未检测，保存设置或点击“重新检测”。</span>;
  }
  if (!status.checkedAt) {
    return <span className="lux-admin-transcoding-state">等待检测结果…</span>;
  }
  if (status.available) {
    return (
      <span className="lux-admin-transcoding-state is-ok">
        <ShieldCheck size={15} aria-hidden="true" /> 自检通过（{new Date(status.checkedAt).toLocaleTimeString()}）
      </span>
    );
  }
  return (
    <span className="lux-admin-transcoding-state is-error">
      <ShieldAlert size={15} aria-hidden="true" />
      自检失败：{status.error ?? "编码器不可用"}
    </span>
  );
}
