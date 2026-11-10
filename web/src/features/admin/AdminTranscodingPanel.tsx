import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Check, RefreshCw, Save, ShieldAlert, ShieldCheck } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type {
  AdminSettings,
  AdminTranscodingSettings,
  AdminTranscodingStatus,
} from "../../lib/api/types";
import { LuxButton, LuxSelectField, LuxSwitch, LuxTextField } from "../../components/ui";

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

type NumericKey = {
  [K in keyof AdminTranscodingSettings]: AdminTranscodingSettings[K] extends number ? K : never;
}[keyof AdminTranscodingSettings];

const ARIA_LABELS: Partial<Record<NumericKey, string>> = {
  remuxSessions: "封装转换并发上限",
  hardwareSessions: "硬件转码并发上限",
  softwareSessions: "软件转码并发上限",
  maxVideoBitrateKbps: "视频码率上限",
  audioBitrateKbps: "音频码率",
  sessionQuotaMb: "单会话磁盘配额",
  globalBudgetMb: "全局磁盘预算",
  minFreeSpaceMb: "最低剩余空间",
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
      <section className="lux-tc-card lux-tc-settings" aria-labelledby="transcoding-settings-heading">
        <header className="lux-tc-card-head">
            <div><h2 id="transcoding-settings-heading">转码设置</h2></div>
        </header>
        <p className="lux-admin-field-hint">当前服务器版本未返回转码设置。</p>
      </section>
    );
  }

  const encoderValues = ["NONE", ...(options?.hardwareEncoders ?? [])];
  const presetValues = options?.softwarePresets ?? ["ultrafast", "superfast", "veryfast", "faster", "fast", "medium"];
  const dirty = JSON.stringify(draft) !== JSON.stringify(current);
  const edit = (patch: Partial<AdminTranscodingSettings>) => {
    setSaved(false);
    setDraft((previous) => (previous ? { ...previous, ...patch } : previous));
  };
  const numeric = (
    key: NumericKey,
    label: string,
    unit: string,
    bounds: { min: number; max: number },
    hint?: string,
  ) => (
    <label className="lux-tc-field">
      <span>{label}</span>
      <div className="lux-admin-input-with-suffix">
        <LuxTextField
          {...numberInput(draft[key], (value) => edit({ [key]: value }), bounds)}
          aria-label={ARIA_LABELS[key] ?? label}
        />
        <em>{unit}</em>
      </div>
      {hint ? <small>{hint}</small> : null}
    </label>
  );

  return (
    <section className="lux-tc-card lux-tc-settings" aria-labelledby="transcoding-settings-heading">
      <header className="lux-tc-card-head">
        <div>
          <h2 id="transcoding-settings-heading">转码设置</h2>
          <p>对之后新开的播放会话生效，进行中的会话保持启动时的参数。</p>
        </div>
      </header>

      <div className="lux-tc-group">
        <LuxSwitch className="lux-admin-toggle" aria-label="启用服务端转码" checked={draft.enabled} onChange={(event) => edit({ enabled: event.target.checked })} label="启用服务端转码" />
        <small>只影响实时播放：关闭后本地媒体只做直连播放或浏览器端解码，不再启动播放转码进程。缩略图、媒体信息探测、章节检测等后台任务仍照常使用 FFmpeg/FFprobe；正在播放的会话不受影响。</small>
      </div>

      <div className="lux-tc-columns">
      <div className="lux-tc-group">
        <h3>编码器</h3>
        <label className="lux-tc-field">
          <span>硬件编码器</span>
          <LuxSelectField
            aria-label="硬件编码器"
            value={draft.hardwareEncoder}
            onChange={(event) => edit({ hardwareEncoder: event.target.value })}
          >
            {encoderValues.map((encoder) => (
              <option key={encoder} value={encoder}>
                {HARDWARE_ENCODER_LABELS[encoder] ?? encoder}
              </option>
            ))}
          </LuxSelectField>
          <small>保存时会自动做一次编码自检；自检不通过的编码器不会用于播放，系统会改用软件转码。部署层 LUX_HLS_HW_ENCODER 优先于此处选择。</small>
        </label>

        <div className="lux-admin-transcoding-status" role="status" aria-label="硬件编码器自检">
          <TranscodingStatusLine status={status} encoder={draft.hardwareEncoder} />
          <LuxButton tone="secondary"
            type="button"
            disabled={probe.isPending || draft.hardwareEncoder === "NONE"}
            onClick={() => probe.mutate()}
          >
            <RefreshCw size={14} className={probe.isPending ? "is-spinning" : undefined} />
            {probe.isPending ? "检测中…" : "重新检测"}
          </LuxButton>
        </div>
        {probe.error ? <p className="lux-error-copy">{probe.error.message}</p> : null}

        <label className="lux-tc-field">
          <span>硬件设备路径</span>
          <LuxTextField
            type="text"
            aria-label="硬件设备路径"
            value={draft.hardwareDevice}
            placeholder="/dev/dri/renderD128"
            onChange={(event) => edit({ hardwareDevice: event.target.value })}
          />
          <small>VAAPI/QSV 使用的 DRM 渲染节点；NVENC 与 VideoToolbox 忽略此值。留空或填写默认值时使用 /dev/dri/renderD128。</small>
        </label>

        <LuxSwitch className="lux-admin-toggle" aria-label="启用硬件解码" checked={draft.hardwareDecode} disabled={!draft.enabled || draft.hardwareEncoder === "NONE"} onChange={(event) => edit({ hardwareDecode: event.target.checked })} label="启用硬件解码" />
        <small>让 FFmpeg 优先使用 GPU 解码（需要容器映射设备并支持对应源格式）；解码不可用或失败时会自动降级到软件转码。仅在启用服务端转码并选择了硬件编码器时生效。</small>
      </div>

      <div className="lux-tc-stack">
      <div className="lux-tc-group">
        <h3>并发上限</h3>
        <div className="lux-tc-grid">
          {numeric("remuxSessions", "封装转换 / 音频转码", "路", { min: 0, max: 16 }, "0 表示禁用这两个档位；同一上限同时约束封装转换（档位 1）与音频转码（档位 2）。")}
          {numeric("hardwareSessions", "硬件转码", "路", { min: 0, max: 16 })}
          {numeric("softwareSessions", "软件转码", "路", { min: 0, max: 16 }, "软件转码占用 CPU 较多，0 表示禁用视频软件转码。")}
        </div>
      </div>

      <div className="lux-tc-group">
        <h3>磁盘保护</h3>
        <div className="lux-tc-grid">
          {numeric("sessionQuotaMb", "单会话磁盘配额", "MB", { min: 0, max: 65_536 }, "0 表示不限制；超出后新分片请求会被拒绝并回收会话。")}
          {numeric("globalBudgetMb", "全局磁盘预算", "MB", { min: 0, max: 1_048_576 }, "所有 HLS 会话合计上限，0 表示不限制；部署层 LUX_HLS_GLOBAL_DISK_BUDGET_BYTES 优先。")}
          {numeric("minFreeSpaceMb", "最低剩余空间", "MB", { min: 0, max: 1_048_576 }, "低于该值时不再启动新的转码会话；0 表示关闭保护。")}
        </div>
      </div>
      </div>

      <div className="lux-tc-group">
        <h3>画质</h3>
        <div className="lux-tc-grid">
          <label className="lux-tc-field">
            <span>软件编码 preset</span>
            <LuxSelectField
              aria-label="软件编码 preset"
              value={draft.softwarePreset}
              onChange={(event) => edit({ softwarePreset: event.target.value })}
            >
              {presetValues.map((preset) => (
                <option key={preset} value={preset}>{PRESET_LABELS[preset] ?? preset}</option>
              ))}
            </LuxSelectField>
          </label>
          <label className="lux-tc-field">
            <span>最大输出分辨率</span>
            <LuxSelectField
              aria-label="最大输出分辨率"
              value={String(draft.maxOutputHeight)}
              onChange={(event) => edit({ maxOutputHeight: Number.parseInt(event.target.value, 10) || 0 })}
            >
              {OUTPUT_HEIGHT_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>{option.label}</option>
              ))}
            </LuxSelectField>
            <small>只缩小、不放大，保持宽高比；封装转换与音频转码不改变画面。</small>
          </label>
          {numeric("maxVideoBitrateKbps", "视频码率上限", "kbps", { min: 0, max: 100_000 }, "0 表示使用编码器默认值；不会把客户端请求的更低码率调高。")}
          {numeric("audioBitrateKbps", "音频码率", "kbps", { min: 32, max: 512 })}
        </div>
      </div>


      </div>

      <footer className="lux-tc-savebar">
        <span className={`lux-tc-savebar-state${dirty ? " is-dirty" : ""}`}>
          {saved ? <><Check size={15} /> 设置已保存，将在新的播放会话生效</> : dirty ? "有未保存的修改" : "已是最新设置"}
        </span>
        <LuxButton tone="primary" className="lux-settings-save"
          type="button"
          disabled={save.isPending}
          onClick={() => save.mutate(draft)}
        >
          <Save size={16} /> {save.isPending ? "保存中…" : "保存转码设置"}
        </LuxButton>
      </footer>
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
