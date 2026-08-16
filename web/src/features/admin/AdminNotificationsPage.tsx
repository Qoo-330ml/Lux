import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { BellRing, Check, Copy, Link2, Plus, RefreshCw, RotateCcw, Send, Trash2, X } from "lucide-react";
import { useState, type FormEvent } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminWebhookDestination, AdminWebhookDelivery } from "../../lib/api/types";
import "./notifications.css";

const EVENT_OPTIONS = [
  ["MEDIA_ADDED", "媒体新增"],
  ["MEDIA_REMOVED", "媒体移除"],
  ["SCAN_COMPLETED", "扫描完成"],
  ["SCAN_FAILED", "扫描失败"],
  ["METADATA_UPDATED", "元数据更新"],
  ["JOB_FAILED", "后台任务失败"],
] as const;

type DestinationForm = {
  name: string;
  url: string;
  enabled: boolean;
  allowPrivateNetwork: boolean;
  eventTypes: string[];
  secret: string;
};

const EMPTY_FORM: DestinationForm = {
  name: "",
  url: "",
  enabled: true,
  allowPrivateNetwork: false,
  eventTypes: [],
  secret: "",
};

export function AdminNotificationsPage() {
  const queryClient = useQueryClient();
  const destinations = useQuery({
    queryKey: queryKeys.adminWebhookDestinations,
    queryFn: () => api.adminWebhookDestinations(),
  });
  const deliveries = useQuery({
    queryKey: queryKeys.adminWebhookDeliveries,
    queryFn: () => api.adminWebhookDeliveries(),
  });
  const [form, setForm] = useState<DestinationForm>(EMPTY_FORM);
  const [secretNotice, setSecretNotice] = useState<string | null>(null);
  const create = useMutation({
    mutationFn: () => api.createAdminWebhookDestination({
      name: form.name.trim(),
      url: form.url.trim(),
      enabled: form.enabled,
      allowPrivateNetwork: form.allowPrivateNetwork,
      eventTypes: form.eventTypes,
      ...(form.secret.trim() ? { secret: form.secret.trim() } : {}),
    }),
    onSuccess: (result) => {
      setForm(EMPTY_FORM);
      setSecretNotice(result.secret);
      void invalidateWebhookQueries(queryClient);
    },
  });

  if (destinations.isPending || deliveries.isPending) {
    return <AdminNotificationsState label="正在读取通知配置…" />;
  }
  if (destinations.error || deliveries.error) {
    return <AdminNotificationsState label={(destinations.error || deliveries.error)?.message || "通知数据加载失败"} error />;
  }

  const destinationItems = destinations.data.destinations ?? [];
  const deliveryItems = deliveries.data.deliveries ?? [];
  return (
    <main className="lux-admin-page lux-notifications-page">
      <header className="lux-admin-page-heading lux-notifications-heading">
        <div><h1>通知</h1><p>把媒体、扫描和后台任务变化发送到受控的外部接收器。</p></div>
        <BellRing size={20} className="lux-admin-panel-icon" />
      </header>
      <section className="lux-admin-panel lux-notifications-create" aria-labelledby="notification-create-title">
        <div className="lux-admin-panel-heading">
          <div><span className="lux-eyebrow">出站通知</span><h2 id="notification-create-title">添加通知目标</h2><p>默认阻止私有网络；只有本地接收器才需要显式打开。</p></div>
          <Plus size={20} className="lux-admin-panel-icon" />
        </div>
        <form className="lux-admin-form lux-notification-form" onSubmit={(event) => { event.preventDefault(); create.mutate(); }}>
          <label htmlFor="notification-name">名称<input id="notification-name" value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} maxLength={128} required /></label>
          <label htmlFor="notification-url">接收地址<input id="notification-url" type="url" value={form.url} onChange={(event) => setForm({ ...form, url: event.target.value })} placeholder="https://example.com/lux-hook" maxLength={2048} required /></label>
          <label htmlFor="notification-secret">Secret（可选）<input id="notification-secret" type="password" value={form.secret} onChange={(event) => setForm({ ...form, secret: event.target.value })} autoComplete="new-password" placeholder="留空由 Lux 生成" /></label>
          <fieldset className="lux-notification-events"><legend>接收事件</legend><div className="lux-notification-event-grid">{EVENT_OPTIONS.map(([value, label]) => <label key={value} htmlFor={`event-${value}`}><input id={`event-${value}`} name={`event-${value}`} type="checkbox" checked={form.eventTypes.includes(value)} onChange={() => setForm({ ...form, eventTypes: toggleEvent(form.eventTypes, value) })} /><span>{label}</span></label>)}</div><small>不勾选表示接收全部当前和未来兼容事件。</small></fieldset>
          <label className="lux-admin-toggle"><input type="checkbox" checked={form.allowPrivateNetwork} onChange={(event) => setForm({ ...form, allowPrivateNetwork: event.target.checked })} /><span>允许私有网络地址（仅限可信本地接收器）</span></label>
          <label className="lux-admin-toggle"><input type="checkbox" checked={form.enabled} onChange={(event) => setForm({ ...form, enabled: event.target.checked })} /><span>创建后立即启用</span></label>
          <button className="lux-button lux-button-primary" type="submit" disabled={create.isPending}><Plus size={16} /> {create.isPending ? "创建中…" : "添加目标"}</button>
        </form>
        {create.error ? <p className="lux-error-copy" role="alert">{create.error.message}</p> : null}
      </section>
      <section className="lux-admin-panel" aria-labelledby="notification-destinations-title">
        <div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">接收器</span><h2 id="notification-destinations-title">通知目标</h2><p>Secret 只在创建或轮换后显示一次，普通列表不会返回。</p></div><span className="lux-status-pill">{destinationItems.length} 个目标</span></div>
        {destinationItems.length === 0 ? <div className="lux-admin-empty"><Link2 size={24} /><h2>还没有通知目标</h2><p>添加一个接收地址后，扫描和媒体变化才会发送出去。</p></div> : <div className="lux-notification-destination-list">{destinationItems.map((destination) => <DestinationRow key={destination.id} destination={destination} onSecret={setSecretNotice} onChanged={() => void invalidateWebhookQueries(queryClient)} />)}</div>}
      </section>
      <DeliveryList deliveries={deliveryItems} onRetry={() => void invalidateWebhookQueries(queryClient)} />
      {secretNotice ? <SecretNotice secret={secretNotice} onClose={() => setSecretNotice(null)} /> : null}
    </main>
  );
}

function DestinationRow({ destination, onSecret, onChanged }: { destination: AdminWebhookDestination; onSecret: (secret: string) => void; onChanged: () => void }) {
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(destination.name);
  const [url, setUrl] = useState(destination.url);
  const [enabled, setEnabled] = useState(destination.enabled);
  const [allowPrivateNetwork, setAllowPrivateNetwork] = useState(destination.allowPrivateNetwork);
  const [eventTypes, setEventTypes] = useState(destination.eventTypes);
  const update = useMutation({ mutationFn: () => api.updateAdminWebhookDestination(destination.id, { name: name.trim(), url: url.trim(), enabled, allowPrivateNetwork, eventTypes }), onSuccess: () => { setEditing(false); onChanged(); } });
  const toggle = useMutation({ mutationFn: (nextEnabled: boolean) => api.updateAdminWebhookDestination(destination.id, { enabled: nextEnabled }), onSuccess: onChanged });
  const test = useMutation({ mutationFn: () => api.testAdminWebhookDestination(destination.id) });
  const rotate = useMutation({ mutationFn: () => api.rotateAdminWebhookSecret(destination.id), onSuccess: (result) => onSecret(result.secret) });
  const remove = useMutation({ mutationFn: () => api.deleteAdminWebhookDestination(destination.id), onSuccess: onChanged });
  const busy = update.isPending || toggle.isPending || test.isPending || rotate.isPending || remove.isPending;
  const error = update.error || toggle.error || test.error || rotate.error || remove.error;
  return <article className={`lux-notification-destination ${destination.enabled ? "is-enabled" : "is-disabled"}`}>
    <div className="lux-notification-destination-summary"><span className="lux-notification-destination-icon"><Link2 size={17} /></span><div><h3>{destination.name}</h3><p>{destination.url}</p><small>{destination.eventTypes.length === 0 ? "全部事件" : destination.eventTypes.map(eventLabel).join(" · ")} · Secret {destination.secretConfigured ? "已配置" : "缺失"}</small></div><span className={destination.enabled ? "lux-user-badge is-ok" : "lux-user-badge is-warn"}>{destination.enabled ? "已启用" : "已停用"}</span></div>
    <div className="lux-notification-destination-actions"><button className="lux-button lux-button-secondary" type="button" onClick={() => setEditing((value) => !value)} disabled={busy}>{editing ? <X size={15} /> : <RefreshCw size={15} />}{editing ? "取消" : "编辑"}</button><button className="lux-button lux-button-secondary" type="button" onClick={() => toggle.mutate(!destination.enabled)} disabled={busy}>{destination.enabled ? "停用" : "启用"}</button><button className="lux-button lux-button-secondary" type="button" onClick={() => test.mutate()} disabled={busy}><Send size={15} />{test.isPending ? "发送中…" : "测试"}</button><button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`轮换 ${destination.name} Secret`} title="轮换 Secret" onClick={() => rotate.mutate()} disabled={busy}><RotateCcw size={15} /></button><button className="lux-icon-button lux-icon-button-small lux-danger-icon" type="button" aria-label={`删除 ${destination.name}`} title="删除通知目标" onClick={() => { if (window.confirm(`确定删除通知目标“${destination.name}”？`)) remove.mutate(); }} disabled={busy}><Trash2 size={15} /></button></div>
    {editing ? <form className="lux-notification-edit-form" onSubmit={(event) => { event.preventDefault(); update.mutate(); }}><label>名称<input value={name} onChange={(event) => setName(event.target.value)} maxLength={128} required /></label><label>接收地址<input type="url" value={url} onChange={(event) => setUrl(event.target.value)} maxLength={2048} required /></label><fieldset className="lux-notification-events"><legend>接收事件</legend><div className="lux-notification-event-grid">{EVENT_OPTIONS.map(([value, label]) => <label key={value}><input type="checkbox" checked={eventTypes.includes(value)} onChange={() => setEventTypes(toggleEvent(eventTypes, value))} /><span>{label}</span></label>)}</div></fieldset><label className="lux-admin-toggle"><input type="checkbox" checked={allowPrivateNetwork} onChange={(event) => setAllowPrivateNetwork(event.target.checked)} /><span>允许私有网络地址</span></label><label className="lux-admin-toggle"><input type="checkbox" checked={enabled} onChange={(event) => setEnabled(event.target.checked)} /><span>启用目标</span></label><button className="lux-button lux-button-primary" type="submit" disabled={update.isPending}>保存修改</button></form> : null}
    {test.data ? <p className="lux-notification-result" role="status">测试发送成功，HTTP {test.data.status}</p> : null}{error ? <p className="lux-error-copy" role="alert">{error.message}</p> : null}
  </article>;
}

function DeliveryList({ deliveries, onRetry }: { deliveries: AdminWebhookDelivery[]; onRetry: () => void }) {
  const retry = useMutation({ mutationFn: (deliveryId: string) => api.retryAdminWebhookDelivery(deliveryId), onSuccess: onRetry });
  return <section className="lux-admin-panel" aria-labelledby="notification-deliveries-title"><div className="lux-admin-panel-heading"><div><span className="lux-eyebrow">投递队列</span><h2 id="notification-deliveries-title">最近投递</h2><p>失败投递会按退避策略自动重试；确认目标恢复后可手动重试。</p></div><span className="lux-status-pill">{deliveries.length} 条记录</span></div>{deliveries.length === 0 ? <p className="lux-admin-muted">暂无投递记录。</p> : <div className="lux-notification-delivery-list">{deliveries.map((delivery) => <article className="lux-notification-delivery" key={delivery.id}><div><strong>{eventLabel(delivery.eventType)}</strong><span>{delivery.destinationName}</span><small>{delivery.lastError || `尝试 ${delivery.attemptCount} 次`}</small></div><span className={delivery.status === "DELIVERED" ? "lux-user-badge is-ok" : delivery.status === "FAILED" ? "lux-user-badge is-warn" : "lux-user-badge"}>{deliveryStatusLabel(delivery.status)}</span>{delivery.status === "FAILED" ? <button className="lux-icon-button lux-icon-button-small" type="button" aria-label={`重试投递 ${delivery.id}`} title="重试投递" onClick={() => retry.mutate(delivery.id)} disabled={retry.isPending}><RotateCcw size={15} /></button> : null}</article>)}</div>}{retry.error ? <p className="lux-error-copy" role="alert">{retry.error.message}</p> : null}</section>;
}

function SecretNotice({ secret, onClose }: { secret: string; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try { await navigator.clipboard.writeText(secret); setCopied(true); } catch { setCopied(false); }
  };
  return <div className="lux-notification-secret-backdrop" role="presentation"><section className="lux-notification-secret" role="dialog" aria-modal="true" aria-labelledby="notification-secret-title"><div className="lux-admin-panel-heading"><div><h2 id="notification-secret-title">请立即保存 Secret</h2><p>关闭后 Lux 不会再次显示这串 Secret；接收方需要用它校验 HMAC 签名。</p></div><button className="lux-icon-button lux-icon-button-small" type="button" aria-label="关闭 Secret 提示" onClick={onClose}><X size={16} /></button></div><code>{secret}</code><div className="lux-notification-secret-actions"><button className="lux-button lux-button-secondary" type="button" onClick={() => void copy()}>{copied ? <Check size={15} /> : <Copy size={15} />}{copied ? "已复制" : "复制 Secret"}</button><button className="lux-button lux-button-primary" type="button" onClick={onClose}>我已保存</button></div></section></div>;
}

function AdminNotificationsState({ label, error = false }: { label: string; error?: boolean }) {
  return <div className="lux-admin-state" role={error ? "alert" : "status"}><BellRing size={20} /><p>{label}</p></div>;
}

function toggleEvent(values: string[], value: string) {
  return values.includes(value) ? values.filter((item) => item !== value) : [...values, value];
}

function eventLabel(value: string) {
  return EVENT_OPTIONS.find(([eventType]) => eventType === value)?.[1] ?? value;
}

function deliveryStatusLabel(value: string) {
  return value === "DELIVERED" ? "已送达" : value === "FAILED" ? "投递失败" : value === "PENDING" ? "等待重试" : value;
}

async function invalidateWebhookQueries(queryClient: ReturnType<typeof useQueryClient>) {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: queryKeys.adminWebhookDestinations }),
    queryClient.invalidateQueries({ queryKey: queryKeys.adminWebhookDeliveries }),
  ]);
}
