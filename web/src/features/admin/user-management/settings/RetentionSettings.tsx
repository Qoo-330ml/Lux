import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FormEvent, useEffect, useState } from "react";
import { api } from "../../../../lib/api/client";
import { queryKeys } from "../../../../lib/api/query-keys";
import { LuxButton, LuxTextField } from "../../../../components/ui";

export function AdminUserRetentionSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [graceDays, setGraceDays] = useState("7");
  const [notice, setNotice] = useState("");
  const update = useMutation({
    mutationFn: (days: number) => api.updateAdminSettings({ userDeletionGraceDays: days }),
    onSuccess: (updated, days) => {
      const savedDays = updated.userDeletionGraceDays ?? days;
      queryClient.setQueryData(queryKeys.adminSettings, { ...updated, userDeletionGraceDays: savedDays });
      setGraceDays(String(savedDays));
      setNotice("删除宽限期已保存。");
    },
    onError: (error) => setNotice(error.message || "保存失败，请重试。"),
  });
  useEffect(() => {
    setGraceDays(String(settings.data?.userDeletionGraceDays ?? 7));
  }, [settings.data?.userDeletionGraceDays]);

  if (settings.isPending) return <p className="lux-admin-muted" role="status">正在读取删除保留设置…</p>;
  if (settings.error) return <p className="lux-error-copy" role="alert">读取删除保留设置失败：{settings.error.message}</p>;

  const parsedDays = Number(graceDays);
  const validDays = Number.isInteger(parsedDays) && parsedDays >= 1 && parsedDays <= 3_650;
  const hasChanges = parsedDays !== (settings.data.userDeletionGraceDays ?? 7);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validDays || !hasChanges || update.isPending) return;
    setNotice("");
    update.mutate(parsedDays);
  }

  return (
    <>
      <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-retention-title">
        <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-retention-title">删除账号保留期限</h2></div></div>
        <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
          <label>
            移入回收站后保留天数
            <LuxTextField
              type="number"
              min={1}
              max={3_650}
              step={1}
              value={graceDays}
              aria-label="删除账号保留天数"
              onChange={(event) => { setGraceDays(event.target.value); setNotice(""); }}
              disabled={update.isPending}
            />
          </label>
          <p className="lux-admin-muted">范围为 1–3650 天。新值只影响之后移入回收站的账号；已在回收站中的账号仍按原计划清理时间处理。</p>
          <div className="lux-admin-user-profile-actions">
            <LuxButton tone="primary" type="submit" disabled={!validDays || !hasChanges || update.isPending}>
              {update.isPending ? "保存中…" : "保存保留期限"}
            </LuxButton>
            {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
          </div>
        </form>
      </section>
      <AdminUserActivityRetentionSettings />
    </>
  );
}

function AdminUserActivityRetentionSettings() {
  const queryClient = useQueryClient();
  const settings = useQuery({ queryKey: queryKeys.adminSettings, queryFn: () => api.adminSettings() });
  const [retentionDays, setRetentionDays] = useState("90");
  const [notice, setNotice] = useState("");
  const update = useMutation({
    mutationFn: (days: number) => api.updateAdminSettings({ userActivityRetentionDays: days }),
    onSuccess: (updated, days) => {
      const savedDays = updated.userActivityRetentionDays ?? days;
      queryClient.setQueryData(queryKeys.adminSettings, { ...updated, userActivityRetentionDays: savedDays });
      setRetentionDays(String(savedDays));
      setNotice("活动记录保留期限已保存。");
    },
    onError: (error) => setNotice(error.message || "保存失败，请重试。"),
  });
  useEffect(() => {
    setRetentionDays(String(settings.data?.userActivityRetentionDays ?? 90));
  }, [settings.data?.userActivityRetentionDays]);

  if (settings.isPending) return <p className="lux-admin-muted" role="status">正在读取活动记录保留设置…</p>;
  if (settings.error) return <p className="lux-error-copy" role="alert">读取活动记录保留设置失败：{settings.error.message}</p>;

  const parsedDays = Number(retentionDays);
  const validDays = Number.isInteger(parsedDays) && parsedDays >= 1 && parsedDays <= 3_650;
  const hasChanges = parsedDays !== (settings.data.userActivityRetentionDays ?? 90);

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!validDays || !hasChanges || update.isPending) return;
    setNotice("");
    update.mutate(parsedDays);
  }

  return (
    <section className="lux-admin-panel lux-admin-user-profile-panel" aria-labelledby="lux-admin-user-activity-retention-title">
      <div className="lux-admin-panel-heading"><div><h2 id="lux-admin-user-activity-retention-title">活动记录保留</h2></div></div>
      <form className="lux-admin-form lux-admin-user-profile-form" onSubmit={submit}>
        <label>
          活动记录保留天数
          <LuxTextField
            type="number"
            min={1}
            max={3_650}
            step={1}
            value={retentionDays}
            aria-label="活动记录保留天数"
            onChange={(event) => { setRetentionDays(event.target.value); setNotice(""); }}
            disabled={update.isPending}
          />
        </label>
        <p className="lux-admin-muted">范围为 1–3650 天。超过期限的用户活动索引会由后台定期清理；观看进度、已看状态、收藏和文件审计不受影响。</p>
        <div className="lux-admin-user-profile-actions">
          <LuxButton tone="primary" type="submit" disabled={!validDays || !hasChanges || update.isPending}>
            {update.isPending ? "保存中…" : "保存活动保留期限"}
          </LuxButton>
          {notice ? <span role={update.isError ? "alert" : "status"}>{notice}</span> : null}
        </div>
      </form>
    </section>
  );
}
