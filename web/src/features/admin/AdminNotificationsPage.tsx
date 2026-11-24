import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  BellRing, Check, Copy, Globe, Link2, Lock, Mail, MessageSquare, MoreHorizontal,
  Pencil, Plus, RotateCcw, Search, Send, Smartphone, Trash2, X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";
import type { AdminPlugin, AdminPluginConfigField, AdminWebhookDelivery, AdminWebhookDestination } from "../../lib/api/types";
import "./notifications.css";
import { LuxButton, LuxCheckbox, LuxIconButton, LuxSelectField, LuxSwitch, LuxTextarea, LuxTextField, useLuxConfirm } from "../../components/ui";

const EVENT_OPTIONS = [
  ["MEDIA_ADDED", "媒体新增"], ["MEDIA_REMOVED", "媒体移除"], ["SCAN_COMPLETED", "扫描完成"],
  ["SCAN_FAILED", "扫描失败"], ["METADATA_UPDATED", "元数据更新"], ["JOB_FAILED", "后台任务失败"],
  ["PLAYBACK_STARTED", "开始播放"], ["PLAYBACK_PAUSED", "暂停播放"], ["PLAYBACK_PROGRESS", "播放进度"],
  ["PLAYBACK_STOPPED", "停止播放"],
] as const;

const DELIVERY_PAGE_SIZE = 20;
const DELIVERY_STATUS_FILTERS = [["", "全部"], ["FAILED", "失败"], ["PENDING", "等待"], ["DELIVERED", "已送达"]] as const;
const MAX_VISIBLE_EVENT_CHIPS = 3;

type DestinationStatusFilter = "all" | "enabled" | "disabled";

type NotificationForm = {
  name: string; url: string; enabled: boolean; allowPrivateNetwork: boolean; eventTypes: string[];
  providerPluginId: string; providerConfig: Record<string, unknown>; secret: string;
};

const EMPTY_FORM: NotificationForm = {
  name: "", url: "", enabled: true, allowPrivateNetwork: false, eventTypes: [],
  providerPluginId: "", providerConfig: {}, secret: "",
};

export function AdminNotificationsPage() {
  const queryClient = useQueryClient();
  const providers = useQuery({ queryKey: queryKeys.adminNotificationProviders, queryFn: () => api.adminNotificationProviders() });
  const destinations = useQuery({ queryKey: queryKeys.adminWebhookDestinations, queryFn: () => api.adminWebhookDestinations() });
  const [tab, setTab] = useState<"destinations" | "deliveries">("destinations");
  const [statusFilter, setStatusFilter] = useState<DestinationStatusFilter>("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [openMenuId, setOpenMenuId] = useState<string | null>(null);
  const [editingDestination, setEditingDestination] = useState<AdminWebhookDestination | null>(null);
  const [deliveryPage, setDeliveryPage] = useState(1);
  const [deliveryStatus, setDeliveryStatus] = useState("");
  const [deliveryDestination, setDeliveryDestination] = useState("");
  const deliveryParams = {
    page: deliveryPage,
    pageSize: DELIVERY_PAGE_SIZE,
    ...(deliveryStatus ? { status: deliveryStatus } : {}),
    ...(deliveryDestination ? { destinationId: deliveryDestination } : {}),
  };
  const deliveries = useQuery({
    queryKey: [...queryKeys.adminWebhookDeliveries, deliveryParams],
    queryFn: () => api.adminWebhookDeliveries(deliveryParams),
    placeholderData: keepPreviousData,
    enabled: tab === "deliveries",
  });
  const [form, setForm] = useState<NotificationForm>(EMPTY_FORM);
  const [createOpen, setCreateOpen] = useState(false);
  const createDialogCloseRef = useRef<HTMLButtonElement>(null);
  const [secretNotice, setSecretNotice] = useState<string | null>(null);
  const closeCreateDialog = useCallback(() => setCreateOpen(false), []);
  const providerItems = useMemo(
    () => (providers.data?.plugins ?? []).filter((plugin) => plugin.installed && plugin.enabled && plugin.available),
    [providers.data?.plugins],
  );
  const selectedProvider = providerItems.find((plugin) => plugin.id === form.providerPluginId) ?? null;
  const createSecretField = selectedProvider ? providerSecretField(selectedProvider) : null;

  useEffect(() => {
    if (providerItems.length === 0 || providerItems.some((plugin) => plugin.id === form.providerPluginId)) return;
    const provider = providerItems[0];
    setForm((current) => ({ ...current, providerPluginId: provider.id, providerConfig: defaultProviderConfig(provider) }));
  }, [form.providerPluginId, providerItems]);

  const create = useMutation({
    mutationFn: () => api.createAdminWebhookDestination({
      name: form.name.trim(),
      url: form.providerPluginId === "builtin.webhook" ? form.url.trim() : "",
      enabled: form.enabled,
      allowPrivateNetwork: form.allowPrivateNetwork,
      eventTypes: form.eventTypes,
      payloadFormat: "LUX",
      providerPluginId: form.providerPluginId,
      providerConfig: form.providerConfig,
      ...(form.secret.trim() ? { secret: form.secret.trim() } : {}),
    }),
    onSuccess: (result) => {
      closeCreateDialog();
      setForm({ ...EMPTY_FORM, providerPluginId: selectedProvider?.id ?? "", providerConfig: selectedProvider ? defaultProviderConfig(selectedProvider) : {} });
      setSecretNotice(result.secret);
      void invalidateNotificationQueries(queryClient);
    },
  });

  useEffect(() => {
    if (!createOpen) return;
    createDialogCloseRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeCreateDialog();
      }
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [closeCreateDialog, createOpen]);

  useEffect(() => {
    if (!openMenuId) return;
    const closeOnPointerDown = (event: PointerEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.closest("[data-notification-menu]")) return;
      setOpenMenuId(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenuId(null);
    };
    document.addEventListener("pointerdown", closeOnPointerDown);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnPointerDown);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [openMenuId]);

  const destinationItems = destinations.data?.destinations ?? [];
  const enabledCount = useMemo(() => destinationItems.filter((item) => item.enabled).length, [destinationItems]);
  const disabledCount = destinationItems.length - enabledCount;
  const filteredDestinations = useMemo(() => {
    const keyword = searchQuery.trim().toLowerCase();
    return destinationItems.filter((item) => {
      if (statusFilter === "enabled" && !item.enabled) return false;
      if (statusFilter === "disabled" && item.enabled) return false;
      if (!keyword) return true;
      const providerName = providerItems.find((p) => p.id === item.providerPluginId)?.name ?? item.providerPluginId;
      const targetUrl = destinationDisplayUrl(item);
      return `${item.name} ${providerName} ${targetUrl}`.toLowerCase().includes(keyword);
    });
  }, [destinationItems, providerItems, searchQuery, statusFilter]);

  if (destinations.error && !destinations.data) return <AdminNotificationsState label={destinations.error.message || "通知目标加载失败"} error />;
  if (destinations.isPending) return <AdminNotificationsState label="正在读取通知目标…" />;

  const deliveryItems = deliveries.data?.deliveries ?? [];

  return (
    <main className="lux-admin-page lux-notifications-page">
      <header className="lux-admin-page-heading lux-notifications-heading">
        <div>
          <h1>通知</h1>
          <p>选择要通知的内容，再选择一个通知器和它的配置。</p>
        </div>
        <BellRing size={20} className="lux-admin-panel-icon" />
      </header>
      {providers.isPending ? <p className="lux-admin-muted" role="status">通知目标已加载，正在读取通知器插件…</p> : null}
      {providers.error ? <p className="lux-error-copy" role="alert">通知器插件加载失败：{providers.error.message}</p> : null}

      {createOpen ? (
        <div
          className="lux-notification-dialog-backdrop"
          role="presentation"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) closeCreateDialog();
          }}
        >
          <section className="lux-notification-dialog" role="dialog" aria-modal="true" aria-labelledby="notification-create-title">
            <div className="lux-notification-dialog-heading">
              <div>
                <span className="lux-eyebrow">新建通知</span>
                <h2 id="notification-create-title">配置通知</h2>
                <p>通知内容由 Lux 管理，实际发送方式由通知器插件提供。</p>
              </div>
              <LuxIconButton className="lux-notification-dialog-close" ref={createDialogCloseRef} type="button" aria-label="关闭新建通知" onClick={closeCreateDialog}>
                <X size={17} />
              </LuxIconButton>
            </div>
            <form
              className="lux-admin-form lux-notification-form"
              onSubmit={(event) => {
                event.preventDefault();
                if (form.providerPluginId) create.mutate();
              }}
            >
              <fieldset className="lux-notification-section">
                <legend>通知内容</legend>
                <div className="lux-notification-event-grid">
                  {EVENT_OPTIONS.map(([value, label]) => (
                    <LuxCheckbox
                      key={value}
                      id={`event-${value}`}
                      name={`event-${value}`}
                      checked={form.eventTypes.includes(value)}
                      onChange={() => setForm({ ...form, eventTypes: toggleEvent(form.eventTypes, value) })}
                      label={<>{label}</>}
                    />
                  ))}
                </div>
                <small>不勾选表示接收全部事件。</small>
              </fieldset>
              <fieldset className="lux-notification-section">
                <legend>通知器</legend>
                {providers.isPending ? (
                  <p className="lux-admin-muted" role="status">正在读取可用通知器…</p>
                ) : providers.error ? (
                  <p className="lux-error-copy" role="alert">通知器暂时不可用。</p>
                ) : providerItems.length === 0 ? (
                  <div className="lux-notification-provider-empty">
                    <p>还没有可用的通知器插件。</p>
                    <a href="/admin/plugins">前往插件库安装通知器</a>
                  </div>
                ) : (
                  <label htmlFor="notification-provider">
                    选择通知器
                    <LuxSelectField
                      id="notification-provider"
                      name="notification-provider"
                      value={form.providerPluginId}
                      onChange={(event) => {
                        const provider = providerItems.find((item) => item.id === event.target.value);
                        setForm({ ...form, providerPluginId: event.target.value, providerConfig: provider ? defaultProviderConfig(provider) : {} });
                      }}
                    >
                      {providerItems.map((provider) => (
                        <option key={provider.id} value={provider.id}>{provider.name}</option>
                      ))}
                    </LuxSelectField>
                  </label>
                )}
                {selectedProvider ? <p className="lux-notification-provider-description">{selectedProvider.description || selectedProvider.id}</p> : null}
              </fieldset>
              {selectedProvider ? (
                <NotificationConfigFields
                  plugin={selectedProvider}
                  values={form.providerConfig}
                  onChange={(providerConfig) => setForm({ ...form, providerConfig })}
                />
              ) : null}
              <fieldset className="lux-notification-section">
                <legend>通知目标</legend>
                <div className="lux-notification-target-grid">
                  <label htmlFor="notification-name">
                    名称
                    <LuxTextField id="notification-name" value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} maxLength={128} required />
                  </label>
                  {form.providerPluginId === "builtin.webhook" ? (
                    <label htmlFor="notification-url">
                      接收地址
                      <LuxTextField
                        id="notification-url"
                        type="url"
                        value={form.url}
                        onChange={(event) => setForm({ ...form, url: event.target.value })}
                        placeholder="https://example.com/lux-hook"
                        maxLength={2048}
                        required
                      />
                    </label>
                  ) : null}
                  {createSecretField ? (
                    <label htmlFor="notification-secret">
                      {createSecretField.label}
                      <LuxTextField
                        id="notification-secret"
                        name="notification-secret"
                        type="password"
                        value={form.secret}
                        onChange={(event) => setForm({ ...form, secret: event.target.value })}
                        autoComplete="new-password"
                        required
                      />
                      <small>{createSecretField.description || "由通知器提供；只保存为这条通知的 Secret，不写入通知器配置，也不会出现在响应或日志中。"}</small>
                    </label>
                  ) : null}
                </div>
                <LuxSwitch
                  className="lux-admin-toggle"
                  checked={form.allowPrivateNetwork}
                  onChange={(event) => setForm({ ...form, allowPrivateNetwork: event.target.checked })}
                  label="允许私有网络地址（仅限可信本地接收器）"
                />
                <LuxSwitch
                  className="lux-admin-toggle"
                  checked={form.enabled}
                  onChange={(event) => setForm({ ...form, enabled: event.target.checked })}
                  label="创建后立即启用"
                />
              </fieldset>
              <LuxButton
                tone="primary"
                type="submit"
                disabled={create.isPending || providerItems.length === 0 || (createSecretField !== null && form.secret.trim() === "")}
              >
                {create.isPending ? "创建中…" : "保存通知"}
              </LuxButton>
            </form>
            {create.error ? <p className="lux-error-copy" role="alert">{create.error.message}</p> : null}
          </section>
        </div>
      ) : null}

      <div className="lux-notification-tabs" role="tablist" aria-label="通知视图">
        <button
          type="button"
          role="tab"
          id="notification-tab-destinations"
          data-tab="destinations"
          aria-selected={tab === "destinations"}
          aria-controls="notification-tabpanel"
          className={tab === "destinations" ? "is-active" : ""}
          onClick={() => setTab("destinations")}
        >
          通知目标<span className="lux-status-pill">{destinationItems.length}</span>
        </button>
        <button
          type="button"
          role="tab"
          id="notification-tab-deliveries"
          data-tab="deliveries"
          aria-selected={tab === "deliveries"}
          aria-controls="notification-tabpanel"
          className={tab === "deliveries" ? "is-active" : ""}
          onClick={() => setTab("deliveries")}
        >
          投递记录
        </button>
      </div>

      <div id="notification-tabpanel" role="tabpanel" aria-labelledby={`notification-tab-${tab}`} className="lux-notification-tabpanel">
        {tab === "destinations" ? (
          <section className="lux-admin-panel lux-notifications-panel" aria-labelledby="notification-destinations-title">
            <h2 id="notification-destinations-title" className="lux-visually-hidden">已配置的通知目标</h2>
            <div className="lux-notification-stats" aria-label="通知概览">
              <div className="lux-notification-stat">
                <span>已配置通知</span>
                <strong>{destinationItems.length}</strong>
              </div>
              <div className="lux-notification-stat">
                <span>运行中渠道</span>
                <strong className={enabledCount > 0 ? "is-ok" : undefined}>{enabledCount} / {destinationItems.length}</strong>
              </div>
              <div className="lux-notification-stat">
                <span>可用通知器</span>
                <strong>{providerItems.length}</strong>
              </div>
            </div>

            <div className="lux-notification-toolbar">
              <div className="lux-notification-filter-pills" role="group" aria-label="按状态筛选通知目标">
                <button
                  type="button"
                  data-filter="all"
                  aria-pressed={statusFilter === "all"}
                  className={statusFilter === "all" ? "is-active" : ""}
                  onClick={() => setStatusFilter("all")}
                >
                  全部 {destinationItems.length}
                </button>
                <button
                  type="button"
                  data-filter="enabled"
                  aria-pressed={statusFilter === "enabled"}
                  className={statusFilter === "enabled" ? "is-active" : ""}
                  onClick={() => setStatusFilter("enabled")}
                >
                  已启用 {enabledCount}
                </button>
                <button
                  type="button"
                  data-filter="disabled"
                  aria-pressed={statusFilter === "disabled"}
                  className={statusFilter === "disabled" ? "is-active" : ""}
                  onClick={() => setStatusFilter("disabled")}
                >
                  已停用 {disabledCount}
                </button>
              </div>
              <div className="lux-notifications-panel-actions">
                <label className="lux-notification-search">
                  <Search size={14} aria-hidden="true" />
                  <input
                    type="search"
                    aria-label="筛选通知目标"
                    placeholder="筛选名称、通知器或地址…"
                    value={searchQuery}
                    onChange={(event) => setSearchQuery(event.target.value)}
                  />
                </label>
                <LuxButton
                  tone="primary"
                  size="compact"
                  type="button"
                  aria-haspopup="dialog"
                  aria-expanded={createOpen}
                  aria-label="新建通知"
                  onClick={() => setCreateOpen(true)}
                >
                  <Plus size={15} aria-hidden="true" /> 新建通知
                </LuxButton>
              </div>
            </div>

            {destinationItems.length === 0 ? (
              <div className="lux-admin-empty">
                <Link2 size={24} />
                <h2>还没有通知</h2>
                <p>配置一个通知后，媒体和后台任务变化才会发送出去。</p>
              </div>
            ) : filteredDestinations.length === 0 ? (
              <div className="lux-notification-empty-filter">
                <p>没有找到匹配的通知目标。</p>
                <LuxButton
                  tone="secondary"
                  size="compact"
                  type="button"
                  onClick={() => {
                    setStatusFilter("all");
                    setSearchQuery("");
                  }}
                >
                  清除筛选
                </LuxButton>
              </div>
            ) : (
              <div className="lux-notification-destination-list">
                {filteredDestinations.map((destination) => (
                  <DestinationRow
                    key={destination.id}
                    destination={destination}
                    providers={providerItems}
                    menuOpen={openMenuId === destination.id}
                    onToggleMenu={() => setOpenMenuId((current) => (current === destination.id ? null : destination.id))}
                    onCloseMenu={() => setOpenMenuId(null)}
                    onEdit={() => {
                      setOpenMenuId(null);
                      setEditingDestination(destination);
                    }}
                    onSecret={setSecretNotice}
                    onChanged={() => void invalidateNotificationQueries(queryClient)}
                  />
                ))}
              </div>
            )}
          </section>
        ) : (
          <DeliveryList
            deliveries={deliveryItems}
            total={deliveries.data?.total ?? deliveryItems.length}
            page={deliveryPage}
            pageSize={DELIVERY_PAGE_SIZE}
            status={deliveryStatus}
            destinationId={deliveryDestination}
            destinations={destinationItems}
            loading={deliveries.isPending}
            error={deliveries.error?.message}
            onPage={setDeliveryPage}
            onStatus={(value) => {
              setDeliveryStatus(value);
              setDeliveryPage(1);
            }}
            onDestination={(value) => {
              setDeliveryDestination(value);
              setDeliveryPage(1);
            }}
            onRetry={() => void invalidateNotificationQueries(queryClient)}
          />
        )}
      </div>

      {editingDestination ? (
        <DestinationEditDialog
          key={editingDestination.id}
          destination={editingDestination}
          providers={providerItems}
          onClose={() => setEditingDestination(null)}
          onSaved={() => {
            setEditingDestination(null);
            void invalidateNotificationQueries(queryClient);
          }}
        />
      ) : null}

      {secretNotice ? <SecretNotice secret={secretNotice} onClose={() => setSecretNotice(null)} /> : null}
    </main>
  );
}

function NotificationConfigFields({ plugin, values, onChange }: { plugin: AdminPlugin; values: Record<string, unknown>; onChange: (values: Record<string, unknown>) => void }) {
  const fields = plugin.configFields.filter((field) => field.key !== "secret");
  if (fields.length === 0) return null;
  return (
    <fieldset className="lux-notification-section lux-notification-config">
      <legend>通知器配置</legend>
      <div className="lux-notification-config-grid">
        {fields.map((field) => (
          <ConfigField key={field.key} field={field} value={fieldValue(field, values)} onChange={(value) => onChange({ ...values, [field.key]: value })} />
        ))}
      </div>
    </fieldset>
  );
}

function ConfigField({ field, value, onChange }: { field: AdminPluginConfigField; value: unknown; onChange: (value: unknown) => void }) {
  const id = `notification-config-${field.key}`;
  const description = field.description ? <small>{field.description}</small> : null;
  if (field.type === "toggle") {
    return (
      <LuxSwitch
        className="lux-admin-toggle lux-notification-config-toggle"
        id={id}
        checked={Boolean(value)}
        onChange={(event) => onChange(event.target.checked)}
        label={field.label}
        description={field.description}
      />
    );
  }
  if (field.type === "select") {
    return (
      <label htmlFor={id}>
        {field.label}
        <LuxSelectField
          id={id}
          name={id}
          multiple={field.multiple}
          value={field.multiple ? (Array.isArray(value) ? value.map(String) : []) : String(value ?? "")}
          onChange={(event) => onChange(field.multiple ? Array.from(event.target.selectedOptions, (option) => option.value) : event.target.value)}
        >
          {field.options?.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
        </LuxSelectField>
        {description}
      </label>
    );
  }
  if (field.type === "textarea") {
    return (
      <label htmlFor={id}>
        {field.label}
        <LuxTextarea id={id} name={id} value={value == null ? "" : String(value)} required={field.required} onChange={(event) => onChange(event.target.value)} />
        {description}
      </label>
    );
  }
  return (
    <label htmlFor={id}>
      {field.label}
      <LuxTextField
        id={id}
        name={id}
        type={field.type === "password" ? "password" : field.type === "number" ? "number" : "text"}
        value={value == null ? "" : String(value)}
        min={field.minimum ?? undefined}
        max={field.maximum ?? undefined}
        required={field.required}
        onChange={(event) => onChange(field.type === "number" ? Number(event.target.value) : event.target.value)}
      />
      {description}
    </label>
  );
}

type DestinationRowProps = {
  destination: AdminWebhookDestination;
  providers: AdminPlugin[];
  menuOpen: boolean;
  onToggleMenu: () => void;
  onCloseMenu: () => void;
  onEdit: () => void;
  onSecret: (secret: string) => void;
  onChanged: () => void;
};

function DestinationRow({
  destination, providers, menuOpen, onToggleMenu, onCloseMenu, onEdit, onSecret, onChanged,
}: DestinationRowProps) {
  const confirm = useLuxConfirm();
  const provider = providers.find((item) => item.id === destination.providerPluginId);
  const secretField = provider ? providerSecretField(provider) : null;
  const toggle = useMutation({
    mutationFn: (nextEnabled: boolean) => api.updateAdminWebhookDestination(destination.id, { enabled: nextEnabled }),
    onSuccess: onChanged,
  });
  const test = useMutation({ mutationFn: () => api.testAdminWebhookDestination(destination.id) });
  const rotate = useMutation({
    mutationFn: () => api.rotateAdminWebhookSecret(destination.id, undefined),
    onSuccess: (result) => {
      onSecret(result.secret);
    },
  });
  const remove = useMutation({
    mutationFn: () => api.deleteAdminWebhookDestination(destination.id),
    onSuccess: onChanged,
  });
  const busy = toggle.isPending || test.isPending || rotate.isPending || remove.isPending;
  const error = toggle.error || test.error || rotate.error || remove.error;
  const displayUrl = destinationDisplayUrl(destination);
  const visibleEvents = destination.eventTypes.slice(0, MAX_VISIBLE_EVENT_CHIPS);
  const extraEvents = destination.eventTypes.slice(MAX_VISIBLE_EVENT_CHIPS);

  return (
    <article className={`lux-notification-destination ${destination.enabled ? "is-enabled" : "is-disabled"}`}>
      <div className="lux-notification-destination-summary">
        <span className="lux-notification-destination-icon" aria-hidden="true">
          {renderProviderIcon(destination.providerPluginId, provider?.name)}
          <span className="lux-notification-status-dot" />
        </span>
        <div className="lux-notification-destination-identity">
          <div className="lux-notification-destination-title">
            <h3>{destination.name}</h3>
            <span className="lux-notification-provider-tag">{provider?.name ?? destination.providerPluginId}</span>
          </div>
          <div className="lux-notification-destination-meta">
            <span className="lux-notification-endpoint" title={displayUrl || undefined}>{displayUrl || "由通知器配置目标"}</span>
            {destination.secretConfigured ? (
              <span className="lux-notification-meta-flag"><Lock size={11} aria-hidden="true" />已配置凭据</span>
            ) : null}
            {destination.allowPrivateNetwork ? (
              <span className="lux-notification-meta-flag"><Globe size={11} aria-hidden="true" />内网</span>
            ) : null}
            {!destination.enabled ? <span className="lux-notification-meta-flag">已停用</span> : null}
          </div>
        </div>
      </div>

      <div className="lux-notification-event-chips" aria-label="订阅事件">
        {destination.eventTypes.length === 0 ? (
          <span className="lux-notification-event-chip is-all">全部事件</span>
        ) : (
          <>
            {visibleEvents.map((eventType) => (
              <span key={eventType} className="lux-notification-event-chip">{eventLabel(eventType)}</span>
            ))}
            {extraEvents.length > 0 ? (
              <span className="lux-notification-event-chip is-more" title={extraEvents.map(eventLabel).join("、")}>
                +{extraEvents.length}
              </span>
            ) : null}
          </>
        )}
      </div>

      <div className="lux-notification-destination-actions">
        <LuxSwitch
          checked={destination.enabled}
          onChange={(event) => toggle.mutate(event.target.checked)}
          disabled={busy}
          aria-label={destination.enabled ? `停用 ${destination.name}` : `启用 ${destination.name}`}
        />
        <span className="lux-notification-actions-sep" aria-hidden="true" />
        <LuxButton tone="secondary" size="compact" type="button" onClick={() => test.mutate()} disabled={busy}>
          <Send size={14} aria-hidden="true" />{test.isPending ? "发送中…" : "测试"}
        </LuxButton>
        <LuxButton tone="secondary" size="compact" type="button" onClick={onEdit} disabled={busy}>
          <Pencil size={14} aria-hidden="true" />编辑
        </LuxButton>
        <div className="lux-notification-menu-wrap" data-notification-menu>
          <LuxIconButton
            size="small"
            type="button"
            aria-label={`更多操作：${destination.name}`}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            onClick={onToggleMenu}
            disabled={busy}
          >
            <MoreHorizontal size={15} aria-hidden="true" />
          </LuxIconButton>
          {menuOpen ? (
            <div className="lux-notification-menu" role="menu">
              <button
                role="menuitem"
                type="button"
                onClick={() => {
                  onCloseMenu();
                  if (secretField) {
                    onEdit();
                  } else {
                    rotate.mutate();
                  }
                }}
              >
                <RotateCcw size={14} aria-hidden="true" />
                {secretField ? "修改凭据…" : "轮换 Secret"}
              </button>
              <div className="lux-notification-menu-divider" role="separator" />
              <button
                role="menuitem"
                type="button"
                className="is-danger"
                onClick={async () => {
                  onCloseMenu();
                  if (await confirm(`确定删除通知“${destination.name}”？`)) remove.mutate();
                }}
              >
                <Trash2 size={14} aria-hidden="true" />
                删除通知
              </button>
            </div>
          ) : null}
        </div>
      </div>

      {test.data || error ? (
        <div className="lux-notification-row-feedback">
          {test.data ? <p className="lux-notification-result" role="status">测试发送成功，HTTP {test.data.status}</p> : null}
          {error ? <p className="lux-error-copy" role="alert">{error.message}</p> : null}
        </div>
      ) : null}
    </article>
  );
}

function DestinationEditDialog({
  destination, providers, onClose, onSaved,
}: {
  destination: AdminWebhookDestination;
  providers: AdminPlugin[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const [name, setName] = useState(destination.name);
  const [url, setUrl] = useState(destination.url);
  const [enabled, setEnabled] = useState(destination.enabled);
  const [allowPrivateNetwork, setAllowPrivateNetwork] = useState(destination.allowPrivateNetwork);
  const [eventTypes, setEventTypes] = useState(destination.eventTypes);
  const [providerPluginId, setProviderPluginId] = useState(destination.providerPluginId);
  const [providerConfig, setProviderConfig] = useState(initialProviderConfig(destination, providers));
  const [secretDraft, setSecretDraft] = useState("");
  const editingProvider = providers.find((item) => item.id === providerPluginId);
  const editingSecretField = editingProvider ? providerSecretField(editingProvider) : null;
  const providerChanged = providerPluginId !== destination.providerPluginId;

  const update = useMutation({
    mutationFn: () => api.updateAdminWebhookDestination(destination.id, {
      name: name.trim(),
      url: providerPluginId === "builtin.webhook" ? url.trim() : "",
      enabled,
      allowPrivateNetwork,
      eventTypes,
      providerPluginId,
      providerConfig,
      ...(editingSecretField && secretDraft.trim() ? { secret: secretDraft.trim() } : {}),
    }),
    onSuccess: () => {
      setSecretDraft("");
      onSaved();
    },
  });

  useEffect(() => {
    closeButtonRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => document.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  return (
    <div
      className="lux-notification-dialog-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section className="lux-notification-dialog" role="dialog" aria-modal="true" aria-labelledby="notification-edit-title">
        <div className="lux-notification-dialog-heading">
          <div>
            <span className="lux-eyebrow">编辑通知</span>
            <h2 id="notification-edit-title">{destination.name}</h2>
            <p>修改通知触发事件、通知器配置及网络/启用状态。</p>
          </div>
          <LuxIconButton className="lux-notification-dialog-close" ref={closeButtonRef} type="button" aria-label="关闭编辑通知" onClick={onClose}>
            <X size={17} />
          </LuxIconButton>
        </div>
        <form
          className="lux-notification-edit-form"
          onSubmit={(event) => {
            event.preventDefault();
            update.mutate();
          }}
        >
          <fieldset className="lux-notification-section">
            <legend>基本信息</legend>
            <div className="lux-notification-target-grid">
              <label>
                名称
                <LuxTextField value={name} onChange={(event) => setName(event.target.value)} maxLength={128} required />
              </label>
              {providerPluginId === "builtin.webhook" ? (
                <label>
                  接收地址
                  <LuxTextField type="url" value={url} onChange={(event) => setUrl(event.target.value)} maxLength={2048} required={Boolean(url)} />
                </label>
              ) : null}
              <label htmlFor="notification-edit-provider">
                通知器
                <LuxSelectField
                  id="notification-edit-provider"
                  name="notification-edit-provider"
                  aria-label="编辑通知器"
                  value={providerPluginId}
                  onChange={(event) => {
                    const next = providers.find((item) => item.id === event.target.value);
                    setProviderPluginId(event.target.value);
                    setProviderConfig(next ? defaultProviderConfig(next) : {});
                    setSecretDraft("");
                  }}
                >
                  {providers.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
                </LuxSelectField>
              </label>
              {editingSecretField ? (
                <label>
                  {providerChanged ? "新凭据（必填）" : "新凭据"}
                  <LuxTextField
                    type="password"
                    value={secretDraft}
                    onChange={(event) => setSecretDraft(event.target.value)}
                    autoComplete="new-password"
                    required={providerChanged}
                    placeholder={providerChanged ? "更换通知器后必须重新填写" : destination.secretConfigured ? "留空表示不修改" : "尚未配置凭据"}
                  />
                  <small>{providerChanged ? "更换通知器后旧凭据不会被沿用。" : "只保存为这条通知的 Secret，不写入通知器配置。"}</small>
                </label>
              ) : null}
            </div>
          </fieldset>

          {editingProvider ? (
            <NotificationConfigFields plugin={editingProvider} values={providerConfig} onChange={setProviderConfig} />
          ) : null}

          <fieldset className="lux-notification-section">
            <legend>通知内容</legend>
            <div className="lux-notification-event-grid">
              {EVENT_OPTIONS.map(([value, label]) => (
                <LuxCheckbox
                  key={value}
                  checked={eventTypes.includes(value)}
                  onChange={() => setEventTypes(toggleEvent(eventTypes, value))}
                  label={<>{label}</>}
                />
              ))}
            </div>
            <small>不勾选表示接收全部事件。</small>
          </fieldset>

          <div className="lux-notification-edit-toggles">
            <LuxSwitch
              className="lux-admin-toggle"
              checked={allowPrivateNetwork}
              onChange={(event) => setAllowPrivateNetwork(event.target.checked)}
              label="允许私有网络地址"
            />
            <LuxSwitch
              className="lux-admin-toggle"
              checked={enabled}
              onChange={(event) => setEnabled(event.target.checked)}
              label="启用通知"
            />
          </div>

          <div className="lux-notification-edit-footer">
            <LuxButton tone="secondary" type="button" onClick={onClose} disabled={update.isPending}>取消</LuxButton>
            <LuxButton
              tone="primary"
              type="submit"
              disabled={update.isPending || Boolean(providerChanged && editingSecretField && secretDraft.trim() === "")}
            >
              {update.isPending ? "保存中…" : "保存修改"}
            </LuxButton>
          </div>
          {update.error ? <p className="lux-error-copy" role="alert">{update.error.message}</p> : null}
        </form>
      </section>
    </div>
  );
}

type DeliveryListProps = {
  deliveries: AdminWebhookDelivery[]; total: number; page: number; pageSize: number; status: string; destinationId: string;
  destinations: AdminWebhookDestination[]; loading: boolean; error?: string;
  onPage: (page: number) => void; onStatus: (status: string) => void; onDestination: (id: string) => void; onRetry: () => void;
};

function DeliveryList({ deliveries, total, page, pageSize, status, destinationId, destinations, loading, error, onPage, onStatus, onDestination, onRetry }: DeliveryListProps) {
  const retry = useMutation({ mutationFn: (deliveryId: string) => api.retryAdminWebhookDelivery(deliveryId), onSuccess: onRetry });
  const pageCount = Math.max(1, Math.ceil(total / pageSize));
  return (
    <section className="lux-admin-panel lux-notification-deliveries" aria-labelledby="notification-deliveries-title">
      <div className="lux-admin-panel-heading">
        <div>
          <span className="lux-eyebrow">投递记录</span>
          <h2 id="notification-deliveries-title">最近发送</h2>
          <p>失败发送会按退避策略自动重试；确认通知器恢复后可手动重试。</p>
        </div>
        <span className="lux-status-pill">共 {total} 条</span>
      </div>
      <div className="lux-notification-delivery-toolbar">
        <div className="lux-notification-status-filter" role="group" aria-label="按状态筛选">
          {DELIVERY_STATUS_FILTERS.map(([value, label]) => (
            <button
              key={value || "all"}
              type="button"
              data-status={value}
              aria-pressed={status === value}
              className={status === value ? "is-active" : ""}
              onClick={() => onStatus(value)}
            >
              {label}
            </button>
          ))}
        </div>
        <LuxSelectField
          id="notification-delivery-destination"
          name="notification-delivery-destination"
          aria-label="按通知目标筛选"
          value={destinationId}
          onChange={(event) => onDestination(event.target.value)}
        >
          <option value="">全部目标</option>
          {destinations.map((destination) => <option key={destination.id} value={destination.id}>{destination.name}</option>)}
        </LuxSelectField>
      </div>
      {loading ? (
        <p className="lux-admin-muted" role="status">正在读取最近发送…</p>
      ) : error ? (
        <p className="lux-error-copy" role="alert">最近发送加载失败：{error}</p>
      ) : deliveries.length === 0 ? (
        <p className="lux-admin-muted">暂无投递记录。</p>
      ) : (
        <div className="lux-notification-table-wrap">
          <table className="lux-notification-delivery-table">
            <thead>
              <tr>
                <th scope="col">状态</th>
                <th scope="col">时间</th>
                <th scope="col">事件</th>
                <th scope="col">内容</th>
                <th scope="col">目标</th>
                <th scope="col"><span className="lux-visually-hidden">操作</span></th>
              </tr>
            </thead>
            <tbody>
              {deliveries.map((delivery) => (
                <tr key={delivery.id} className={`is-${delivery.status.toLowerCase()}`}>
                  <td>
                    <span className={delivery.status === "DELIVERED" ? "lux-user-badge is-ok" : delivery.status === "FAILED" ? "lux-user-badge is-warn" : "lux-user-badge"}>
                      {deliveryStatusLabel(delivery.status)}
                    </span>
                  </td>
                  <td className="lux-notification-delivery-time">
                    <time dateTime={new Date(delivery.createdAt * 1000).toISOString()}>{formatDeliveryTime(delivery.createdAt)}</time>
                  </td>
                  <td>{eventLabel(delivery.eventType)}</td>
                  <td className="lux-notification-delivery-cell" title={delivery.content || undefined}>
                    <strong>{delivery.title || eventLabel(delivery.eventType)}</strong>
                    {delivery.content ? <span>{delivery.content.replace(/\s+/g, " ")}</span> : null}
                    {delivery.status !== "DELIVERED" ? <small>{delivery.lastError || `尝试 ${delivery.attemptCount} 次`}</small> : null}
                  </td>
                  <td>{delivery.destinationName}</td>
                  <td>
                    {delivery.status === "FAILED" ? (
                      <LuxIconButton
                        className="lux-icon-button-small"
                        type="button"
                        aria-label={`重试投递 ${delivery.id}`}
                        title="重试发送"
                        onClick={() => retry.mutate(delivery.id)}
                        disabled={retry.isPending}
                      >
                        <RotateCcw size={15} />
                      </LuxIconButton>
                    ) : null}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {retry.error ? <p className="lux-error-copy" role="alert">{retry.error.message}</p> : null}
      <nav className="lux-notification-pagination" aria-label="投递记录分页">
        <span>第 {page} / {pageCount} 页 · 共 {total} 条</span>
        <div>
          <LuxButton type="button" aria-label="上一页" disabled={page <= 1} onClick={() => onPage(page - 1)}>上一页</LuxButton>
          <LuxButton type="button" aria-label="下一页" disabled={page >= pageCount} onClick={() => onPage(page + 1)}>下一页</LuxButton>
        </div>
      </nav>
    </section>
  );
}

function SecretNotice({ secret, onClose }: { secret: string; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(secret);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };
  return (
    <div className="lux-notification-secret-backdrop" role="presentation">
      <section className="lux-notification-secret" role="dialog" aria-modal="true" aria-labelledby="notification-secret-title">
        <div className="lux-admin-panel-heading">
          <div>
            <h2 id="notification-secret-title">请立即保存 Secret</h2>
            <p>关闭后 Lux 不会再次显示这串 Secret；接收方需要用它校验 HMAC 签名。</p>
          </div>
          <LuxIconButton className="lux-icon-button-small" type="button" aria-label="关闭 Secret 提示" onClick={onClose}>
            <X size={16} />
          </LuxIconButton>
        </div>
        <code>{secret}</code>
        <div className="lux-notification-secret-actions">
          <LuxButton tone="secondary" type="button" onClick={() => void copy()}>
            {copied ? <Check size={15} /> : <Copy size={15} />}{copied ? "已复制" : "复制 Secret"}
          </LuxButton>
          <LuxButton tone="primary" type="button" onClick={onClose}>我已保存</LuxButton>
        </div>
      </section>
    </div>
  );
}

function AdminNotificationsState({ label, error = false }: { label: string; error?: boolean }) {
  return <div className="lux-admin-state" role={error ? "alert" : "status"}><BellRing size={20} /><p>{label}</p></div>;
}

function renderProviderIcon(pluginId: string, name?: string) {
  const key = `${pluginId} ${name ?? ""}`.toLowerCase();
  if (key.includes("bark") || key.includes("apns") || key.includes("ios") || key.includes("gotify") || key.includes("push")) {
    return <Smartphone size={17} />;
  }
  if (key.includes("telegram")) return <Send size={17} />;
  if (key.includes("mail") || key.includes("smtp")) return <Mail size={17} />;
  if (key.includes("discord") || key.includes("slack") || key.includes("wecom") || key.includes("dingtalk") || key.includes("feishu")) {
    return <MessageSquare size={17} />;
  }
  return <Link2 size={17} />;
}

function destinationDisplayUrl(destination: AdminWebhookDestination): string {
  if (destination.url) return destination.url;
  return typeof destination.providerConfig.url === "string" ? destination.providerConfig.url : "";
}

function providerOwnsTarget(plugin: AdminPlugin) { return plugin.configFields.some((field) => field.key === "url"); }
function providerSecretField(plugin: AdminPlugin) { return plugin.secretField ?? null; }
function defaultProviderConfig(plugin: AdminPlugin) {
  return Object.fromEntries(plugin.configFields.filter((field) => field.defaultValue !== undefined).map((field) => [field.key, field.defaultValue]));
}
function initialProviderConfig(destination: AdminWebhookDestination, providers: AdminPlugin[]) {
  const provider = providers.find((item) => item.id === destination.providerPluginId);
  const config = { ...destination.providerConfig };
  if (provider && providerOwnsTarget(provider) && config.url === undefined && destination.url) config.url = destination.url;
  return config;
}
function fieldValue(field: AdminPluginConfigField, values: Record<string, unknown>) {
  if (values[field.key] !== undefined) return values[field.key];
  if (field.defaultValue !== undefined) return field.defaultValue;
  return field.multiple ? [] : field.type === "toggle" ? false : "";
}
function toggleEvent(values: string[], value: string) {
  return values.includes(value) ? values.filter((item) => item !== value) : [...values, value];
}
function eventLabel(value: string) { return EVENT_OPTIONS.find(([eventType]) => eventType === value)?.[1] ?? value; }
function formatDeliveryTime(seconds: number) {
  return new Date(seconds * 1000).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit", hour12: false });
}
function deliveryStatusLabel(value: string) {
  return value === "DELIVERED" ? "已送达" : value === "FAILED" ? "发送失败" : value === "PENDING" ? "等待重试" : value;
}
async function invalidateNotificationQueries(queryClient: ReturnType<typeof useQueryClient>) {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: queryKeys.adminNotificationProviders }),
    queryClient.invalidateQueries({ queryKey: queryKeys.adminWebhookDestinations }),
    queryClient.invalidateQueries({ queryKey: queryKeys.adminWebhookDeliveries }),
  ]);
}
