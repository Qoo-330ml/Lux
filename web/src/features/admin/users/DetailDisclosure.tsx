import { ChevronDown } from "lucide-react";
import { useState, type ReactNode } from "react";

type DetailDisclosureProps = {
  id: string;
  /** 展开按钮文案，例如「编辑资料」。 */
  openLabel: string;
  /** 收起按钮文案，例如「收起编辑」。 */
  closeLabel: string;
  /** 收起状态下仍然展示的一行说明。 */
  hint?: string;
  children: ReactNode;
};

/**
 * 详情页里「只在用到时才编辑」的折叠区：默认收起，保留所在卡片已展示的信息。
 * 折叠内容始终挂载但带 hidden，保证 aria-controls 指向可用节点。
 */
export function DetailDisclosure({ id, openLabel, closeLabel, hint, children }: DetailDisclosureProps) {
  const [open, setOpen] = useState(false);
  const bodyId = `${id}-body`;
  return (
    <div className="lux-users-disclosure">
      <div className="lux-users-disclosure-head">
        {hint ? <p className="lux-admin-muted">{hint}</p> : null}
        <button
          className="lux-users-disclosure-toggle"
          type="button"
          aria-expanded={open}
          aria-controls={bodyId}
          onClick={() => setOpen((value) => !value)}
        >
          {open ? closeLabel : openLabel}
          <ChevronDown size={15} aria-hidden="true" className={open ? "is-open" : undefined} />
        </button>
      </div>
      <div className="lux-users-disclosure-body" id={bodyId} hidden={!open}>
        {children}
      </div>
    </div>
  );
}
