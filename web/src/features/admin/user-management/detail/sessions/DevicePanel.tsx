import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../../../../../lib/api/client";
import type { AdminUser } from "../../../../../lib/api/types";
import { LuxButton } from "../../../../../components/ui";
import { AdminUserManagementState } from "../../AdminUserManagementState";
import { WebSessionDate } from "./WebSessionDate";

export function AdminUserDevicePanel({ user }: { user: AdminUser }) {
  const [page, setPage] = useState(1);
  const devicesQuery = useQuery({
    queryKey: ["admin", "user", user.id, "devices", page],
    queryFn: () => api.adminUserDevices(user.id, page, 20),
  });
  const devicePage = devicesQuery.data;
  const pageCount = devicePage
    ? Math.max(1, Math.ceil(devicePage.totalCount / devicePage.pageSize))
    : 1;

  if (devicesQuery.isPending) {
    return <AdminUserManagementState label="正在读取设备清单…" />;
  }
  if (devicesQuery.error) {
    return (
      <section className="lux-admin-panel lux-admin-user-sessions" aria-labelledby="lux-admin-user-devices-title">
        <div className="lux-admin-panel-heading">
          <div><h2 id="lux-admin-user-devices-title">设备清单</h2></div>
        </div>
        <p role="alert">读取设备清单失败：{devicesQuery.error.message}</p>
        <LuxButton type="button" onClick={() => void devicesQuery.refetch()}>重试</LuxButton>
      </section>
    );
  }
  if (!devicePage) {
    return <AdminUserManagementState label="正在读取设备清单…" />;
  }

  return (
    <section className="lux-admin-panel lux-admin-user-sessions lux-admin-user-devices" aria-labelledby="lux-admin-user-devices-title">
      <div className="lux-admin-panel-heading">
        <div>
          <h2 id="lux-admin-user-devices-title">设备清单</h2>
          <p className="lux-admin-user-session-summary">{devicePage.totalCount.toLocaleString()} 台已记录设备。设备标识由客户端提供，可能随客户端数据清除而变化。</p>
        </div>
      </div>
      {devicePage.devices.length === 0 ? (
        <p className="lux-admin-muted" role="status">尚无已记录的设备。</p>
      ) : (
        <div className="lux-admin-user-session-table-wrap">
          <table className="lux-admin-user-session-table lux-admin-user-device-table">
            <thead>
              <tr>
                <th scope="col">客户端 / 设备</th>
                <th scope="col">登录状态</th>
                <th scope="col">类型</th>
                <th scope="col">首次出现</th>
                <th scope="col">最近活动</th>
                <th scope="col">最近展示 IP / 归属地</th>
              </tr>
            </thead>
            <tbody>
              {devicePage.devices.map((device) => {
                const locationSummary = [
                  device.lastRemoteIpLocation?.country,
                  device.lastRemoteIpLocation?.province,
                  device.lastRemoteIpLocation?.city,
                  device.lastRemoteIpLocation?.isp,
                ]
                  .map((value) => value?.trim())
                  .filter((value): value is string => Boolean(value))
                  .join(" · ") || "未知";
                return (
                  <tr key={device.deviceId}>
                    <th scope="row">
                      {device.clientName}
                      <span className="lux-admin-user-session-device-meta">
                        {device.deviceName || "未命名设备"} · ID：{device.deviceId}
                        {device.clientVersion ? ` · 版本 ${device.clientVersion}` : ""}
                      </span>
                    </th>
                    <td>
                      <span className={`lux-admin-device-login-status ${device.hasActiveLogin ? "is-active" : "is-inactive"}`}>
                        {device.hasActiveLogin ? "有有效登录" : "无有效登录"}
                      </span>
                    </td>
                    <td>{device.deviceType || "—"}</td>
                    <td><WebSessionDate value={device.firstSeenAt} /></td>
                    <td><WebSessionDate value={device.lastSeenAt} /></td>
                    <td>
                      <span>{device.lastRemoteIp || "—"}</span>
                      <span className="lux-admin-user-session-device-meta">{locationSummary}</span>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <nav className="lux-admin-user-session-pagination" aria-label="设备清单分页">
        <span>第 {devicePage.page} / {pageCount} 页</span>
        <div>
          <LuxButton
            type="button"
            disabled={devicesQuery.isFetching || devicePage.page <= 1}
            onClick={() => setPage((currentPage) => Math.max(1, currentPage - 1))}
          >上一页</LuxButton>
          <LuxButton
            type="button"
            disabled={devicesQuery.isFetching || devicePage.page >= pageCount}
            onClick={() => setPage((currentPage) => currentPage + 1)}
          >下一页</LuxButton>
        </div>
      </nav>
    </section>
  );
}
