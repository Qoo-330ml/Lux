import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Save, Settings2 } from "lucide-react";
import { useEffect, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";

export function AdminSettingsPage() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [playedPercent, setPlayedPercent] = useState("90");
  const [minimumMinutes, setMinimumMinutes] = useState("2");
  const [saved, setSaved] = useState(false);
  useEffect(() => { if (settings.data) { setPlayedPercent(String(settings.data.resumePlayedPercent)); setMinimumMinutes(String(Math.round(settings.data.resumeMinTicks / 600000000))); } }, [settings.data]);
  const save = useMutation({ mutationFn: () => api.updateAdminSettings({ resumePlayedPercent: Number(playedPercent), resumeMinTicks: Number(minimumMinutes) * 600000000 }), onSuccess: (data) => { setPlayedPercent(String(data.resumePlayedPercent)); setMinimumMinutes(String(Math.round(data.resumeMinTicks / 600000000))); setSaved(true); void queryClient.invalidateQueries({ queryKey: queryKeys.adminSettings }); }, onError: () => setSaved(false) });

  if (settings.isPending) return <AdminSettingsState label="正在读取服务器设置…" />;
  if (settings.error) return <AdminSettingsState label={settings.error.message} error />;
  return <div className="lux-admin-page"><header className="lux-admin-page-heading"><div><span className="lux-eyebrow">SERVER SETTINGS</span><h1>服务器设置</h1><p>调整播放状态和用户体验相关的全局策略。</p></div></header><section className="lux-admin-panel lux-admin-settings-panel"><div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">PLAYBACK</span><h2>播放行为</h2></div><Settings2 size={20} className="lux-admin-panel-icon" /></div><div className="lux-admin-settings-form"><label><span>标记为已看</span><small>播放进度达到此百分比后，媒体会自动标记为已看。</small><div className="lux-admin-input-with-suffix"><input type="number" min="1" max="100" value={playedPercent} onChange={(event) => { setSaved(false); setPlayedPercent(event.target.value); }} /><em>%</em></div></label><label><span>继续观看最小进度</span><small>低于此播放时长的记录不会显示在“继续观看”。</small><div className="lux-admin-input-with-suffix"><input type="number" min="0" value={minimumMinutes} onChange={(event) => { setSaved(false); setMinimumMinutes(event.target.value); }} /><em>分钟</em></div></label><button className="lux-button lux-button-primary lux-settings-save" type="button" disabled={save.isPending} onClick={() => save.mutate()}><Save size={16} /> {save.isPending ? "保存中…" : "保存设置"}</button></div>{saved ? <p className="lux-settings-saved"><Check size={15} /> 设置已保存</p> : null}{save.error ? <p className="lux-error-copy">{save.error.message}</p> : null}</section><section className="lux-admin-panel lux-admin-settings-note"><div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">ABOUT THIS SERVER</span><h2>服务信息</h2></div></div><p>媒体扫描、元数据任务和数据库健康状态可在控制台的对应页面查看。Lux Web 与管理 API 使用同源会话和 CSRF 保护。</p></section></div>;
}

function AdminSettingsState({ label, error = false }: { label: string; error?: boolean }) { return <section className="lux-admin-page-state" role={error ? "alert" : "status"}><span className="lux-eyebrow">LUX ADMIN</span><h1>{error ? "设置加载失败" : "正在加载设置"}</h1><p>{label}</p></section>; }
