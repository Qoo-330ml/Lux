import { Copy, KeyRound, RefreshCw, Trash2 } from "lucide-react";
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api/client";
import { queryKeys } from "../../lib/api/query-keys";

export function AdminApiKeyPanel() {
  const queryClient = useQueryClient();
  const [notice, setNotice] = useState<string | null>(null);
  const apiKey = useQuery({ queryKey: queryKeys.adminApiKey, queryFn: () => api.adminApiKey() });
  const rotate = useMutation({
    mutationFn: () => api.rotateAdminApiKey(),
    onSuccess: (data) => {
      queryClient.setQueryData(queryKeys.adminApiKey, data);
      setNotice("API Key 已轮换，旧 Key 已立即失效。");
    },
    onError: (error) => setNotice(error instanceof Error ? error.message : "API Key 轮换失败，请重试。"),
  });
  const revoke = useMutation({
    mutationFn: () => api.revokeAdminApiKey(),
    onSuccess: () => {
      queryClient.setQueryData(queryKeys.adminApiKey, { configured: false, apiKey: null });
      setNotice("API Key 已撤销。");
    },
    onError: (error) => setNotice(error instanceof Error ? error.message : "API Key 撤销失败，请重试。"),
  });

  const currentKey = apiKey.data?.apiKey ?? null;
  const busy = apiKey.isPending || rotate.isPending || revoke.isPending;

  const copyKey = async () => {
    if (!currentKey || !navigator.clipboard) {
      setNotice("当前浏览器不支持复制，请手动复制 Key。");
      return;
    }
    try {
      await navigator.clipboard.writeText(currentKey);
      setNotice("API Key 已复制到剪贴板。");
    } catch {
      setNotice("复制失败，请手动复制 Key。");
    }
  };

  const rotateKey = () => {
    if (!currentKey || window.confirm("轮换后所有使用旧 API Key 的脚本都会立即失效，继续吗？")) {
      setNotice(null);
      rotate.mutate();
    }
  };

  const revokeKey = () => {
    if (window.confirm("撤销后所有使用该 API Key 的脚本都会立即失效，继续吗？")) {
      setNotice(null);
      revoke.mutate();
    }
  };

  return (
    <div className="lux-account-api-key-panel" aria-labelledby="admin-api-key-heading">
      <div className="lux-setting-block-heading">
        <div>
          <strong id="admin-api-key-heading"><KeyRound size={16} aria-hidden="true" />共享管理员 API Key</strong>
          <p>兼容 Emby API Key，可调用 Lux 和 Emby 接口。所有管理员看到同一个 Key。</p>
        </div>
        <span className="lux-setting-hint">高权限凭据</span>
      </div>
      <div className="lux-account-api-key-warning" role="note">
        持有此 Key 等同于拥有服务器管理员权限。不要提交到代码仓库、聊天记录或不受信任的服务。
      </div>
      {apiKey.isPending ? <p className="lux-account-api-key-status" role="status">正在读取 API Key…</p> : null}
      {apiKey.error ? <p className="lux-error-copy" role="alert">API Key 暂时无法读取：{apiKey.error.message}</p> : null}
      {!apiKey.isPending && !apiKey.error ? (
        currentKey ? (
          <div className="lux-account-api-key-value">
            <code>{currentKey}</code>
            <div className="lux-account-api-key-actions">
              <button className="lux-button lux-button-secondary" type="button" onClick={() => void copyKey()} disabled={busy}>
                <Copy size={15} />复制 Key
              </button>
              <button className="lux-button lux-button-secondary" type="button" onClick={rotateKey} disabled={busy}>
                <RefreshCw size={15} />轮换
              </button>
              <button className="lux-button lux-button-danger" type="button" onClick={revokeKey} disabled={busy}>
                <Trash2 size={15} />撤销
              </button>
            </div>
          </div>
        ) : (
          <button className="lux-button lux-button-secondary lux-account-api-key-generate" type="button" onClick={rotateKey} disabled={busy}>
            <KeyRound size={15} />生成共享 API Key
          </button>
        )
      ) : null}
      {notice ? <p className="lux-account-notice" role="status">{notice}</p> : null}
    </div>
  );
}
