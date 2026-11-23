import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  Ban,
  BookOpen,
  Check,
  Clock,
  Eye,
  EyeOff,
  FolderOpen,
  Globe,
  HardDrive,
  KeyRound,
  Library,
  Pencil,
  RefreshCw,
  Search,
  ShieldCheck,
  Trash2,
  UserMinus,
  UserPlus,
  Users,
} from "lucide-react";
import * as React from "react";
import { createPortal } from "react-dom";
import { usersApi } from "@/api/endpoints";
import type { UserAdminItem } from "@/api/types";
import { cn } from "@/lib/cn";
import { formatRelativeTime } from "@/lib/format";
import { useAuthStore } from "@/stores/auth";
import { notify } from "@/stores/toast";
import { Button, IconButton } from "@/ui/Button";
import { Badge, EmptyView, Segmented, Spinner, Switch } from "@/ui/Controls";
import { Input } from "@/ui/Input";
import { AnimatePresence, motion, SPRING_POP, SPRING_SNAPPY, useReducedMotion } from "@/ui/motion";
import { Sheet } from "@/ui/Sheet";

function errText(err: unknown): string {
  return err instanceof Error ? err.message : "操作失败";
}

/* ==========================================================================
   Shared danger confirmation (also used by SettingsPage)
   ========================================================================== */

export type ConfirmTone = "danger" | "warn" | "accent";

export interface ConfirmDialogProps {
  open: boolean;
  onClose: () => void;
  /** 返回 Promise 时，对话框会等待其完成；reject 时保持打开 */
  onConfirm: () => void | Promise<unknown>;
  title: React.ReactNode;
  description?: React.ReactNode;
  /** 明确的后果列表 */
  consequences?: React.ReactNode[];
  confirmLabel?: string;
  cancelLabel?: string;
  tone?: ConfirmTone;
  busy?: boolean;
  /** 高危操作：需手动输入该文本才可确认 */
  confirmText?: string;
  confirmDisabled?: boolean;
  icon?: React.ReactNode;
  children?: React.ReactNode;
}

const TONE_ICON_CLASS: Record<ConfirmTone, string> = {
  danger: "bg-danger-soft text-danger",
  warn: "bg-warn-soft text-warn",
  accent: "bg-accent-soft text-accent",
};

const TONE_CONFIRM_CLASS: Record<ConfirmTone, string> = {
  danger: "bg-danger text-white hover:brightness-110",
  warn: "bg-warn text-white hover:brightness-110",
  accent: "bg-accent text-accent-ink hover:brightness-110",
};

/** 居中确认对话框（移动端底部弹出），带后果说明与可选的输入校验 */
export function ConfirmDialog({
  open,
  onClose,
  onConfirm,
  title,
  description,
  consequences,
  confirmLabel = "确认",
  cancelLabel = "取消",
  tone = "danger",
  busy = false,
  confirmText,
  confirmDisabled = false,
  icon,
  children,
}: ConfirmDialogProps) {
  const reduced = useReducedMotion();
  const [typed, setTyped] = React.useState("");
  const [pending, setPending] = React.useState(false);
  const cancelRef = React.useRef<HTMLButtonElement>(null);
  const typeRef = React.useRef<HTMLInputElement>(null);
  const titleId = React.useId();

  const working = busy || pending;

  React.useEffect(() => {
    if (!open) return;
    setTyped("");
    setPending(false);
    const t = window.setTimeout(() => {
      (confirmText ? typeRef.current : cancelRef.current)?.focus();
    }, 40);
    return () => window.clearTimeout(t);
  }, [open, confirmText]);

  React.useEffect(() => {
    if (!open) return;
    // capture 阶段拦截 Esc，避免同时关闭下层抽屉
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        if (!working) onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onClose, working]);

  const textOk = !confirmText || typed.trim() === confirmText;
  const canConfirm = textOk && !confirmDisabled && !working;

  const handleConfirm = async () => {
    if (!canConfirm) return;
    try {
      setPending(true);
      await onConfirm();
      setPending(false);
      onClose();
    } catch {
      // 错误由调用方提示；保持打开以便重试
      setPending(false);
    }
  };

  if (typeof document === "undefined") return null;

  return createPortal(
    <AnimatePresence>
      {open ? (
        <div
          className="fixed inset-0 z-[70] flex items-end justify-center p-0 sm:items-center sm:p-4"
          role="alertdialog"
          aria-modal="true"
          aria-labelledby={titleId}
        >
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.18 }}
            onClick={() => {
              if (!working) onClose();
            }}
            className="fixed inset-0 bg-black/30 backdrop-blur-[2px] dark:bg-black/55"
          />
          <motion.div
            initial={reduced ? { opacity: 0 } : { opacity: 0, y: 24, scale: 0.97 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={reduced ? { opacity: 0 } : { opacity: 0, y: 16, scale: 0.98 }}
            transition={reduced ? { duration: 0.12 } : SPRING_POP}
            className="surface-sheet safe-b relative z-10 w-full overflow-hidden rounded-t-3xl sm:max-w-[420px] sm:rounded-2xl"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex gap-3.5 px-5 pt-5 pb-4">
              <div
                className={cn(
                  "flex size-10 shrink-0 items-center justify-center rounded-2xl",
                  TONE_ICON_CLASS[tone],
                )}
              >
                {icon ?? <AlertTriangle className="size-[18px]" />}
              </div>
              <div className="min-w-0 flex-1">
                <h2 id={titleId} className="font-serif text-base font-semibold tracking-tight text-ink">
                  {title}
                </h2>
                {description ? (
                  <div className="mt-1 text-xs leading-relaxed text-ink-secondary">{description}</div>
                ) : null}
                {consequences && consequences.length > 0 ? (
                  <ul className="mt-3 flex flex-col gap-1.5 rounded-xl border border-line bg-subtle/55 px-3 py-2.5">
                    {consequences.map((c, i) => (
                      <li key={i} className="flex items-start gap-2 text-[11.5px] leading-relaxed text-ink-secondary">
                        <span
                          className={cn(
                            "mt-[7px] size-1 shrink-0 rounded-full",
                            tone === "danger" ? "bg-danger" : tone === "warn" ? "bg-warn" : "bg-accent",
                          )}
                        />
                        <span className="min-w-0">{c}</span>
                      </li>
                    ))}
                  </ul>
                ) : null}
                {children ? <div className="mt-3">{children}</div> : null}
                {confirmText ? (
                  <div className="mt-3 flex flex-col gap-1.5">
                    <label className="text-[11px] text-ink-muted">
                      请输入 <span className="font-mono font-semibold text-ink">{confirmText}</span> 以确认
                    </label>
                    <input
                      ref={typeRef}
                      value={typed}
                      onChange={(e) => setTyped(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") void handleConfirm();
                      }}
                      autoComplete="off"
                      spellCheck={false}
                      className={cn(
                        "h-9 rounded-xl border bg-elevated px-3 font-mono text-[13px] text-ink outline-none transition-colors",
                        typed && !textOk
                          ? "border-danger/60"
                          : "border-line-strong/80 focus:border-accent/60 focus:ring-2 focus:ring-accent/15",
                      )}
                    />
                  </div>
                ) : null}
              </div>
            </div>
            <div className="flex items-center justify-end gap-2 border-t border-line bg-elevated/55 px-5 py-3">
              <button
                ref={cancelRef}
                type="button"
                disabled={working}
                onClick={onClose}
                className="h-8 cursor-pointer rounded-full px-3.5 text-xs font-medium text-ink-secondary outline-none transition-colors hover:bg-subtle hover:text-ink focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-50"
              >
                {cancelLabel}
              </button>
              <button
                type="button"
                disabled={!canConfirm}
                onClick={() => void handleConfirm()}
                className={cn(
                  "inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-full px-3.5 text-xs font-medium outline-none transition-[filter,opacity] focus-visible:ring-2 focus-visible:ring-accent/60 disabled:cursor-not-allowed disabled:opacity-45",
                  TONE_CONFIRM_CLASS[tone],
                )}
              >
                {working ? (
                  <span className="size-3.5 animate-spin rounded-full border-2 border-current border-t-transparent" />
                ) : null}
                {working ? "处理中…" : confirmLabel}
              </button>
            </div>
          </motion.div>
        </div>
      ) : null}
    </AnimatePresence>,
    document.body,
  );
}

/** 触发器 + 确认对话框（自管理打开状态） */
export function ConfirmAction({
  trigger,
  ...dialog
}: Omit<ConfirmDialogProps, "open" | "onClose"> & {
  trigger: (open: () => void) => React.ReactNode;
}) {
  const [open, setOpen] = React.useState(false);
  const close = React.useCallback(() => setOpen(false), []);
  return (
    <>
      {trigger(() => setOpen(true))}
      <ConfirmDialog {...dialog} open={open} onClose={close} />
    </>
  );
}

/* ==========================================================================
   Quota & permission visualisation
   ========================================================================== */

function quotaRatio(used: number, limit: number): number {
  if (limit <= 0) return 0;
  return Math.min(1, Math.max(0, used / limit));
}

function QuotaBar({
  icon,
  label,
  used,
  limit,
  compact = false,
}: {
  icon: React.ReactNode;
  label: string;
  used: number;
  limit: number;
  compact?: boolean;
}) {
  const unlimited = limit <= 0;
  const ratio = quotaRatio(used, limit);
  const over = !unlimited && used > limit;
  const tone = unlimited
    ? "bg-ink-muted/35"
    : ratio >= 1
      ? "bg-danger"
      : ratio >= 0.8
        ? "bg-warn"
        : "bg-accent";
  return (
    <div className={cn("flex min-w-0 flex-col", compact ? "gap-1" : "gap-1.5")}>
      <div className="flex items-center justify-between gap-2 text-[11px] tabular-nums">
        <span className="inline-flex items-center gap-1 text-ink-muted">
          {icon}
          {label}
        </span>
        <span className={cn("font-medium", over ? "text-danger" : ratio >= 0.8 ? "text-warn" : "text-ink-secondary")}>
          {used}
          <span className="text-ink-muted"> / {unlimited ? "不限" : limit}</span>
        </span>
      </div>
      <div
        className={cn(
          "relative h-1.5 w-full overflow-hidden rounded-full",
          unlimited
            ? "bg-[repeating-linear-gradient(135deg,var(--color-line)_0_4px,transparent_4px_8px)]"
            : "bg-subtle",
        )}
        role="meter"
        aria-label={`${label}配额`}
        aria-valuemin={0}
        aria-valuemax={unlimited ? undefined : limit}
        aria-valuenow={used}
      >
        {!unlimited ? (
          <motion.div
            initial={false}
            animate={{ width: `${Math.max(used > 0 ? 3 : 0, ratio * 100)}%` }}
            transition={SPRING_SNAPPY}
            className={cn("h-full rounded-full", tone)}
          />
        ) : null}
      </div>
    </div>
  );
}

function PermChip({ on, icon, label }: { on: boolean; icon: React.ReactNode; label: string }) {
  return (
    <span
      title={`${label}：${on ? "已开启" : "已关闭"}`}
      className={cn(
        "inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] font-medium leading-none",
        on
          ? "border-success/20 bg-success-soft text-success"
          : "border-line bg-subtle/60 text-ink-muted line-through decoration-ink-muted/60",
      )}
    >
      {icon}
      {label}
    </span>
  );
}

/* ==========================================================================
   Users management
   ========================================================================== */

interface UserDraft {
  mode: "add" | "edit";
  username: string;
  password: string;
  isAdmin: boolean;
  disabled: boolean;
  enableBookSource: boolean;
  enableWebdav: boolean;
  enableLocalStore: boolean;
  bookLimit: number;
  bookSourceLimit: number;
}

const EMPTY_USER_DRAFT: UserDraft = {
  mode: "add",
  username: "",
  password: "",
  isAdmin: false,
  disabled: false,
  enableBookSource: true,
  enableWebdav: true,
  enableLocalStore: true,
  bookLimit: 0,
  bookSourceLimit: 0,
};

const QUOTA_PRESETS = [0, 100, 300, 1000] as const;
const DAY_MS = 86_400_000;

type UserFilter = "all" | "admin" | "disabled" | "near" | "inactive";

function isNearQuota(u: UserAdminItem): boolean {
  return quotaRatio(u.bookCount, u.bookLimit) >= 0.8 || quotaRatio(u.bookSourceCount, u.bookSourceLimit) >= 0.8;
}

function isInactive(u: UserAdminItem, days = 30): boolean {
  return (u.lastLoginAt || 0) < Date.now() - days * DAY_MS;
}

function validateUsername(raw: string): string | null {
  const username = raw.trim();
  if (!username) return null;
  if (username.length < 5 || !/^[A-Za-z0-9]+$/.test(username)) {
    return "用户名需不少于 5 位，且仅包含英文字母与数字";
  }
  if (username.toLowerCase() === "default") return "不能使用保留用户名 default";
  return null;
}

function draftDirty(a: UserDraft | null, b: UserDraft | null): boolean {
  if (!a || !b) return false;
  return (Object.keys(a) as (keyof UserDraft)[]).some((k) => a[k] !== b[k]);
}

export function UsersSection() {
  const qc = useQueryClient();
  const currentUsername = useAuthStore((s) => s.user?.username ?? "");

  const [keyword, setKeyword] = React.useState("");
  const [filter, setFilter] = React.useState<UserFilter>("all");
  const [draft, setDraft] = React.useState<UserDraft | null>(null);
  const [draftBase, setDraftBase] = React.useState<UserDraft | null>(null);
  const [discardAsk, setDiscardAsk] = React.useState(false);
  const [resetTarget, setResetTarget] = React.useState<string | null>(null);
  const [newPassword, setNewPassword] = React.useState("");
  const [confirmPassword, setConfirmPassword] = React.useState("");
  const [showPwd, setShowPwd] = React.useState(false);
  const [showInactiveBar, setShowInactiveBar] = React.useState(false);
  const [inactiveDays, setInactiveDays] = React.useState("30");
  const [inactiveAsk, setInactiveAsk] = React.useState(false);

  const usersQuery = useQuery({
    queryKey: ["settings", "users"],
    queryFn: () => usersApi.list(),
  });

  const users = React.useMemo(() => usersQuery.data ?? [], [usersQuery.data]);

  const stats = React.useMemo(
    () => ({
      total: users.length,
      admins: users.filter((u) => u.isAdmin).length,
      disabled: users.filter((u) => u.disabled).length,
      near: users.filter(isNearQuota).length,
      inactive: users.filter((u) => isInactive(u)).length,
      active7: users.filter((u) => (u.lastLoginAt || 0) >= Date.now() - 7 * DAY_MS).length,
    }),
    [users],
  );

  const filteredUsers = React.useMemo(() => {
    const q = keyword.trim().toLowerCase();
    return users.filter((u) => {
      if (q && !u.username.toLowerCase().includes(q)) return false;
      if (filter === "admin") return u.isAdmin;
      if (filter === "disabled") return u.disabled;
      if (filter === "near") return isNearQuota(u);
      if (filter === "inactive") return isInactive(u);
      return true;
    });
  }, [users, keyword, filter]);

  const inactiveDayNum = Math.max(0, Number.parseInt(inactiveDays, 10) || 0);
  const inactiveCandidates = React.useMemo(
    () => users.filter((u) => u.username !== currentUsername && isInactive(u, inactiveDayNum)),
    [users, currentUsername, inactiveDayNum],
  );

  const invalidate = () => qc.invalidateQueries({ queryKey: ["settings", "users"] });

  const saveMutation = useMutation({
    mutationFn: async (d: UserDraft) => {
      if (d.mode === "add") {
        const username = d.username.trim();
        if (username.length < 5 || !/^[A-Za-z0-9]+$/.test(username)) {
          throw new Error("用户名需不少于 5 位，且仅包含英文字母与数字");
        }
        if (username.toLowerCase() === "default") {
          throw new Error("不能使用保留用户名 default");
        }
        if (!d.password.trim()) {
          throw new Error("请填写初始密码");
        }
        await usersApi.add({
          username,
          password: d.password,
          isAdmin: d.isAdmin,
          enableBookSource: d.enableBookSource,
          enableWebdav: d.enableWebdav,
          enableLocalStore: d.enableLocalStore,
          bookLimit: Math.max(0, Number(d.bookLimit) || 0),
          bookSourceLimit: Math.max(0, Number(d.bookSourceLimit) || 0),
        });
      } else {
        await usersApi.update({
          username: d.username,
          isAdmin: d.isAdmin,
          disabled: d.disabled,
          enableBookSource: d.enableBookSource,
          enableWebdav: d.enableWebdav,
          enableLocalStore: d.enableLocalStore,
          bookLimit: Math.max(0, Number(d.bookLimit) || 0),
          bookSourceLimit: Math.max(0, Number(d.bookSourceLimit) || 0),
        });
      }
    },
    onSuccess: (_data, vars) => {
      notify.success(vars.mode === "add" ? `已创建用户 ${vars.username}` : `已更新用户 ${vars.username}`);
      setDraft(null);
      setDraftBase(null);
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const resetPwdMutation = useMutation({
    mutationFn: ({ username, password }: { username: string; password: string }) => {
      if (!password.trim()) {
        return Promise.reject(new Error("请输入新密码"));
      }
      return usersApi.resetPassword(username, password);
    },
    onSuccess: (_data, vars) => {
      notify.success(`已重置 ${vars.username} 的密码`);
      setResetTarget(null);
      setNewPassword("");
      setConfirmPassword("");
    },
    onError: (err) => notify.error(errText(err)),
  });

  const removeMutation = useMutation({
    mutationFn: (username: string) => usersApi.remove(username),
    onSuccess: (_data, username) => {
      notify.success(`已删除用户 ${username}`);
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const clearInactiveMutation = useMutation({
    mutationFn: (days: number) => usersApi.clearInactive(days),
    onSuccess: (res) => {
      notify.success(
        res.count > 0 ? `已清理 ${res.count} 个不活跃账号` : "没有符合条件的不活跃账号",
      );
      setShowInactiveBar(false);
      void invalidate();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const openDraft = (d: UserDraft) => {
    setDraft(d);
    setDraftBase(d);
  };

  const openEdit = (item: UserAdminItem) => {
    openDraft({
      mode: "edit",
      username: item.username,
      password: "",
      isAdmin: item.isAdmin,
      disabled: item.disabled,
      enableBookSource: item.enableBookSource,
      enableWebdav: item.enableWebdav,
      enableLocalStore: item.enableLocalStore,
      bookLimit: item.bookLimit,
      bookSourceLimit: item.bookSourceLimit,
    });
  };

  const dirty = draftDirty(draft, draftBase);

  const forceCloseDraft = React.useCallback(() => {
    setDraft(null);
    setDraftBase(null);
    setDiscardAsk(false);
  }, []);

  const requestCloseDraft = () => {
    if (saveMutation.isPending) return;
    if (dirty) setDiscardAsk(true);
    else forceCloseDraft();
  };

  const closeReset = React.useCallback(() => {
    setResetTarget(null);
    setNewPassword("");
    setConfirmPassword("");
    setShowPwd(false);
  }, []);

  const editingItem = draft?.mode === "edit" ? users.find((u) => u.username === draft.username) : undefined;
  const editingSelf = draft?.mode === "edit" && draft.username === currentUsername;
  const usernameError = draft?.mode === "add" ? validateUsername(draft.username) : null;
  const canSave =
    !!draft &&
    !saveMutation.isPending &&
    (draft.mode === "add"
      ? !!draft.username.trim() && !usernameError && !!draft.password.trim()
      : dirty);

  const pwdMismatch = confirmPassword.length > 0 && confirmPassword !== newPassword;

  const filterOptions: { value: UserFilter; label: string; badge?: React.ReactNode }[] = [
    { value: "all", label: "全部" },
    { value: "admin", label: "管理员", badge: <FilterCount n={stats.admins} /> },
    { value: "disabled", label: "已停用", badge: <FilterCount n={stats.disabled} /> },
    { value: "near", label: "接近上限", badge: <FilterCount n={stats.near} /> },
    { value: "inactive", label: "30 天未登录", badge: <FilterCount n={stats.inactive} /> },
  ];

  return (
    <section
      id="settings-users"
      data-settings-section=""
      data-label="用户管理"
      className="flex scroll-mt-32 flex-col gap-4 md:scroll-mt-6"
    >
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h2 className="font-serif text-[17px] font-semibold tracking-tight text-ink">用户管理</h2>
            <Badge>{users.length} 人</Badge>
          </div>
          <p className="mt-1 text-xs leading-relaxed text-ink-muted">
            管理多用户账号的角色、功能权限与书籍 / 书源配额
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant={showInactiveBar ? "subtle" : "ghost"}
            size="sm"
            onClick={() => setShowInactiveBar((v) => !v)}
          >
            <UserMinus className="size-3.5" />
            清理不活跃
          </Button>
          <IconButton
            size="sm"
            aria-label="刷新用户列表"
            onClick={() => void usersQuery.refetch()}
          >
            <RefreshCw className={cn("size-3.5", usersQuery.isFetching && "animate-spin")} />
          </IconButton>
          <Button variant="primary" size="sm" onClick={() => openDraft({ ...EMPTY_USER_DRAFT })}>
            <UserPlus className="size-3.5" />
            新增用户
          </Button>
        </div>
      </header>

      {/* Overview stats */}
      {users.length > 0 ? (
        <div className="grid grid-cols-2 gap-2.5 tabular-nums sm:grid-cols-4">
          <StatTile label="账号总数" value={stats.total} icon={<Users className="size-3.5" />} />
          <StatTile
            label="管理员"
            value={stats.admins}
            icon={<ShieldCheck className="size-3.5" />}
            tone="accent"
          />
          <StatTile
            label="7 天内活跃"
            value={stats.active7}
            icon={<Clock className="size-3.5" />}
            tone="success"
          />
          <StatTile
            label="已停用 / 近上限"
            value={`${stats.disabled} / ${stats.near}`}
            icon={<AlertTriangle className="size-3.5" />}
            tone={stats.disabled + stats.near > 0 ? "warn" : "default"}
          />
        </div>
      ) : null}

      {/* Inline cleanup strip */}
      <AnimatePresence initial={false}>
        {showInactiveBar ? (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: "auto" }}
            exit={{ opacity: 0, height: 0 }}
            transition={SPRING_SNAPPY}
            className="overflow-hidden"
          >
            <div className="flex flex-col gap-2.5 rounded-2xl border border-warn/25 bg-warn-soft/40 px-4 py-3">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div className="flex flex-wrap items-center gap-2 text-xs text-ink-secondary">
                  <span>清理超过</span>
                  <input
                    type="number"
                    min={0}
                    max={3650}
                    value={inactiveDays}
                    onChange={(e) => setInactiveDays(e.target.value)}
                    className="h-7 w-18 rounded-lg border border-line-strong bg-elevated px-2 text-center text-xs font-medium text-ink tabular-nums outline-none focus:border-accent"
                  />
                  <span>天未登录的非当前账号</span>
                </div>
                <div className="flex items-center gap-2">
                  <Button variant="ghost" size="xs" onClick={() => setShowInactiveBar(false)}>
                    取消
                  </Button>
                  <Button variant="danger" size="xs" onClick={() => setInactiveAsk(true)}>
                    执行清理
                  </Button>
                </div>
              </div>
              <p className="text-[11px] text-ink-muted">
                预计影响{" "}
                <span className="font-semibold text-warn tabular-nums">{inactiveCandidates.length}</span> 个账号
                {inactiveCandidates.length > 0
                  ? `：${inactiveCandidates
                      .slice(0, 6)
                      .map((u) => u.username)
                      .join("、")}${inactiveCandidates.length > 6 ? " 等" : ""}`
                  : ""}
              </p>
            </div>
          </motion.div>
        ) : null}
      </AnimatePresence>

      {/* Filter + search */}
      {users.length > 0 ? (
        <div className="flex flex-col gap-2.5 md:flex-row md:items-center md:justify-between">
          <div className="-mx-1 max-w-full overflow-x-auto px-1 no-scrollbar">
            <Segmented
              layoutId="v2-users-filter"
              size="sm"
              value={filter}
              onChange={setFilter}
              options={filterOptions}
            />
          </div>
          {users.length > 4 ? (
            <Input
              size="sm"
              value={keyword}
              onChange={(e) => setKeyword(e.target.value)}
              clearable
              onClear={() => setKeyword("")}
              leftIcon={<Search className="size-3.5" />}
              placeholder="按用户名筛选…"
              className="w-full md:w-56"
            />
          ) : null}
        </div>
      ) : null}

      {usersQuery.isLoading ? (
        <div className="flex justify-center py-10">
          <Spinner label="加载用户列表…" />
        </div>
      ) : filteredUsers.length === 0 ? (
        <div className="rounded-2xl border border-dashed border-line-strong bg-subtle/25">
          <EmptyView
            compact
            icon={<Users className="size-5" />}
            title={keyword || filter !== "all" ? "没有匹配的用户" : "暂无其他用户"}
            description={
              keyword || filter !== "all" ? "请尝试调整筛选条件或搜索词" : "点击右上角「新增用户」创建账号"
            }
            action={
              keyword || filter !== "all" ? (
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() => {
                    setKeyword("");
                    setFilter("all");
                  }}
                >
                  清除筛选
                </Button>
              ) : undefined
            }
          />
        </div>
      ) : (
        <div className="divide-y divide-line/70 border-y border-line/70 md:divide-line md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated">
          {filteredUsers.map((item) => {
            const isSelf = item.username === currentUsername;
            return (
              <div
                key={item.username}
                className={cn(
                  "group px-0.5 py-3.5 transition-colors hover:bg-subtle/35 md:px-4",
                  item.disabled && "bg-subtle/25",
                )}
              >
                <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                  <div className="flex min-w-0 flex-1 items-start gap-3">
                    <div className="relative shrink-0">
                      <div
                        className={cn(
                          "flex size-10 items-center justify-center rounded-full font-serif text-sm font-semibold uppercase",
                          item.disabled
                            ? "bg-subtle text-ink-muted"
                            : item.isAdmin
                              ? "bg-accent-soft text-accent ring-2 ring-accent/25"
                              : "bg-subtle text-ink-secondary",
                        )}
                      >
                        {item.username.slice(0, 1)}
                      </div>
                      {item.isAdmin ? (
                        <span
                          title="管理员"
                          className="absolute -right-0.5 -bottom-0.5 flex size-4 items-center justify-center rounded-full bg-accent text-accent-ink ring-2 ring-elevated"
                        >
                          <ShieldCheck className="size-2.5" />
                        </span>
                      ) : null}
                    </div>

                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-1.5">
                        <span
                          className={cn(
                            "truncate text-sm font-medium",
                            item.disabled ? "text-ink-muted" : "text-ink",
                          )}
                        >
                          {item.username}
                        </span>
                        {item.isAdmin ? (
                          <Badge tone="accent">
                            <ShieldCheck className="size-3" />
                            管理员
                          </Badge>
                        ) : null}
                        {isSelf ? <Badge tone="success">当前</Badge> : null}
                        {item.disabled ? (
                          <Badge tone="danger">
                            <Ban className="size-3" />
                            已停用
                          </Badge>
                        ) : null}
                      </div>

                      <p className="mt-0.5 text-[11px] text-ink-muted tabular-nums">
                        最近登录 {formatRelativeTime(item.lastLoginAt) || "从未"}
                        {item.createdAt ? ` · 创建于 ${formatRelativeTime(item.createdAt)}` : ""}
                      </p>

                      <div className="mt-2.5 grid grid-cols-1 gap-x-5 gap-y-2 sm:max-w-[420px] sm:grid-cols-2">
                        <QuotaBar
                          compact
                          icon={<Library className="size-3" />}
                          label="书架"
                          used={item.bookCount}
                          limit={item.bookLimit}
                        />
                        <QuotaBar
                          compact
                          icon={<Globe className="size-3" />}
                          label="书源"
                          used={item.bookSourceCount}
                          limit={item.bookSourceLimit}
                        />
                      </div>

                      <div className="mt-2.5 flex flex-wrap items-center gap-1">
                        <PermChip on={item.enableBookSource} icon={<BookOpen className="size-3" />} label="书源搜索" />
                        <PermChip on={item.enableWebdav} icon={<HardDrive className="size-3" />} label="WebDAV" />
                        <PermChip
                          on={item.enableLocalStore}
                          icon={<FolderOpen className="size-3" />}
                          label="本地书仓"
                        />
                      </div>
                    </div>
                  </div>

                  <div className="flex shrink-0 items-center gap-1 self-end sm:self-start">
                    <Button variant="ghost" size="xs" onClick={() => openEdit(item)}>
                      <Pencil className="size-3" />
                      编辑
                    </Button>
                    <Button
                      variant="ghost"
                      size="xs"
                      onClick={() => {
                        setResetTarget(item.username);
                        setNewPassword("");
                        setConfirmPassword("");
                      }}
                    >
                      <KeyRound className="size-3" />
                      改密
                    </Button>
                    {!isSelf ? (
                      <ConfirmAction
                        title={`删除用户 ${item.username}？`}
                        description="此操作不可撤销，请确认已备份该用户的重要数据。"
                        consequences={[
                          `将永久删除该账号及其书架（${item.bookCount} 本）、书源（${item.bookSourceCount} 个）、书签与阅读进度`,
                          "该用户的登录会话立即失效，无法再登录",
                          "用户名释放后可被重新注册",
                        ]}
                        confirmText={item.username}
                        confirmLabel="永久删除"
                        icon={<Trash2 className="size-[18px]" />}
                        busy={removeMutation.isPending}
                        onConfirm={() => removeMutation.mutateAsync(item.username)}
                        trigger={(open) => (
                          <IconButton
                            size="sm"
                            tone="danger"
                            aria-label={`删除用户 ${item.username}`}
                            onClick={open}
                          >
                            <Trash2 className="size-3.5" />
                          </IconButton>
                        )}
                      />
                    ) : null}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Clear inactive confirmation */}
      <ConfirmDialog
        open={inactiveAsk}
        onClose={() => setInactiveAsk(false)}
        title={`清理 ${inactiveDayNum} 天未登录的账号？`}
        description="清理将由服务端按最近登录时间判定，当前登录账号不会被清理。"
        consequences={[
          `预计删除 ${inactiveCandidates.length} 个账号及其全部书架、书源与阅读数据`,
          "删除后无法恢复，相关用户需重新注册",
        ]}
        confirmLabel="确认清理"
        confirmText={inactiveCandidates.length > 0 ? "清理" : undefined}
        icon={<UserMinus className="size-[18px]" />}
        busy={clearInactiveMutation.isPending}
        onConfirm={() => clearInactiveMutation.mutateAsync(inactiveDayNum)}
      />

      {/* Reset password dialog */}
      <ConfirmDialog
        open={resetTarget !== null}
        onClose={closeReset}
        tone="warn"
        icon={<KeyRound className="size-[18px]" />}
        title={`重置 ${resetTarget ?? ""} 的密码`}
        description="设置新密码后请通过安全渠道告知该用户。"
        consequences={["旧密码立即失效，该用户需使用新密码重新登录"]}
        confirmLabel="保存新密码"
        busy={resetPwdMutation.isPending}
        confirmDisabled={!newPassword.trim() || newPassword !== confirmPassword}
        onConfirm={() =>
          resetTarget
            ? resetPwdMutation.mutateAsync({ username: resetTarget, password: newPassword })
            : undefined
        }
      >
        <div className="flex flex-col gap-2.5">
          <div className="relative">
            <Input
              size="sm"
              type={showPwd ? "text" : "password"}
              autoComplete="new-password"
              value={newPassword}
              onChange={(e) => setNewPassword(e.target.value)}
              placeholder="输入新密码"
            />
            <button
              type="button"
              aria-label={showPwd ? "隐藏密码" : "显示密码"}
              onClick={() => setShowPwd((v) => !v)}
              className="absolute top-1/2 right-2 flex size-6 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full text-ink-muted hover:bg-subtle hover:text-ink"
            >
              {showPwd ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
            </button>
          </div>
          <Input
            size="sm"
            type={showPwd ? "text" : "password"}
            autoComplete="new-password"
            value={confirmPassword}
            onChange={(e) => setConfirmPassword(e.target.value)}
            placeholder="再次输入以确认"
            error={pwdMismatch ? "两次输入的密码不一致" : undefined}
            onKeyDown={(e) => {
              if (
                e.key === "Enter" &&
                resetTarget &&
                newPassword.trim() &&
                newPassword === confirmPassword &&
                !resetPwdMutation.isPending
              ) {
                resetPwdMutation.mutate({ username: resetTarget, password: newPassword });
              }
            }}
          />
        </div>
      </ConfirmDialog>

      {/* Add / Edit User Sheet */}
      <Sheet
        open={draft !== null}
        onClose={requestCloseDraft}
        size="md"
        title={draft?.mode === "add" ? "新增用户" : `编辑用户 · ${draft?.username ?? ""}`}
        subtitle={
          draft?.mode === "add"
            ? "用户名不少于 5 位且仅支持英文字母与数字"
            : "调整角色、功能权限、配额上限或账号状态"
        }
        footer={
          draft ? (
            <div className="flex items-center justify-between gap-3">
              <SaveState pending={saveMutation.isPending} dirty={dirty} mode={draft.mode} />
              <div className="flex items-center gap-2">
                <Button variant="ghost" size="sm" onClick={requestCloseDraft} disabled={saveMutation.isPending}>
                  取消
                </Button>
                <Button
                  variant="primary"
                  size="sm"
                  loading={saveMutation.isPending}
                  disabled={!canSave}
                  onClick={() => saveMutation.mutate(draft)}
                >
                  {draft.mode === "add" ? "创建用户" : "保存更改"}
                </Button>
              </div>
            </div>
          ) : null
        }
      >
        {draft ? (
          <div className="flex flex-col gap-6">
            {/* Group: identity */}
            <FormGroup title="账号信息">
              {draft.mode === "add" ? (
                <div className="flex flex-col gap-3.5">
                  <Input
                    label="用户名"
                    placeholder="例如 alice01（≥5位字母数字）"
                    autoComplete="off"
                    value={draft.username}
                    error={usernameError ?? undefined}
                    onChange={(e) => setDraft({ ...draft, username: e.target.value })}
                  />
                  <Input
                    label="初始密码"
                    type="password"
                    autoComplete="new-password"
                    placeholder="请输入密码"
                    value={draft.password}
                    onChange={(e) => setDraft({ ...draft, password: e.target.value })}
                  />
                </div>
              ) : (
                <div className="flex items-center gap-3 rounded-2xl border border-line bg-elevated px-4 py-3">
                  <div className="flex size-10 shrink-0 items-center justify-center rounded-full bg-accent-soft font-serif text-sm font-semibold text-accent uppercase">
                    {draft.username.slice(0, 1)}
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="truncate text-sm font-medium text-ink">{draft.username}</span>
                      {editingSelf ? <Badge tone="success">当前</Badge> : null}
                    </div>
                    <p className="mt-0.5 text-[11px] text-ink-muted tabular-nums">
                      最近登录 {formatRelativeTime(editingItem?.lastLoginAt) || "从未"}
                      {editingItem?.createdAt ? ` · 创建于 ${formatRelativeTime(editingItem.createdAt)}` : ""}
                    </p>
                  </div>
                </div>
              )}
            </FormGroup>

            {/* Group: role & status */}
            <FormGroup title="角色与状态">
              <div className="divide-y divide-line rounded-2xl border border-line bg-elevated">
                <ToggleRow
                  icon={<ShieldCheck className="size-4 text-accent" />}
                  title="管理员权限"
                  desc="可管理用户与发布公共书源"
                  checked={draft.isAdmin}
                  onChange={(v) => setDraft({ ...draft, isAdmin: v })}
                />
                {draft.mode === "edit" ? (
                  <ToggleRow
                    icon={<Ban className="size-4 text-danger" />}
                    title="停用该账号"
                    titleClassName="text-danger"
                    desc={editingSelf ? "不能停用当前登录的账号" : "停用后无法登录及调用接口"}
                    checked={draft.disabled}
                    disabled={editingSelf && !draft.disabled}
                    onChange={(v) => setDraft({ ...draft, disabled: v })}
                  />
                ) : null}
              </div>
              <AnimatePresence initial={false}>
                {editingSelf && draftBase?.isAdmin && !draft.isAdmin ? (
                  <Callout key="self-admin" tone="warn">
                    你正在取消自己的管理员权限，保存后将无法再访问用户管理。
                  </Callout>
                ) : null}
                {draft.mode === "edit" && draft.disabled && !draftBase?.disabled ? (
                  <Callout key="disable" tone="danger">
                    保存后该用户将被立即登出且无法再登录，数据会被保留，可随时重新启用。
                  </Callout>
                ) : null}
              </AnimatePresence>
            </FormGroup>

            {/* Group: permissions */}
            <FormGroup title="功能权限">
              <div className="divide-y divide-line rounded-2xl border border-line bg-elevated">
                <ToggleRow
                  icon={<BookOpen className="size-4 text-ink-muted" />}
                  title="允许使用书源搜索"
                  desc="开启在线搜书与发现能力"
                  checked={draft.enableBookSource}
                  onChange={(v) => setDraft({ ...draft, enableBookSource: v })}
                />
                <ToggleRow
                  icon={<HardDrive className="size-4 text-ink-muted" />}
                  title="允许 WebDAV 备份恢复"
                  desc="读写专属备份文件仓"
                  checked={draft.enableWebdav}
                  onChange={(v) => setDraft({ ...draft, enableWebdav: v })}
                />
                <ToggleRow
                  icon={<FolderOpen className="size-4 text-ink-muted" />}
                  title="允许访问本地书仓"
                  desc="读取公共本地存储目录"
                  checked={draft.enableLocalStore}
                  onChange={(v) => setDraft({ ...draft, enableLocalStore: v })}
                />
              </div>
            </FormGroup>

            {/* Group: quota */}
            <FormGroup title="配额上限" hint="0 表示不限制">
              <div className="flex flex-col gap-3">
                <QuotaField
                  label="书籍数量上限"
                  icon={<Library className="size-3" />}
                  value={draft.bookLimit}
                  used={editingItem?.bookCount}
                  onChange={(v) => setDraft({ ...draft, bookLimit: v })}
                />
                <QuotaField
                  label="书源数量上限"
                  icon={<Globe className="size-3" />}
                  value={draft.bookSourceLimit}
                  used={editingItem?.bookSourceCount}
                  onChange={(v) => setDraft({ ...draft, bookSourceLimit: v })}
                />
              </div>
            </FormGroup>
          </div>
        ) : null}
      </Sheet>

      {/* Unsaved changes guard */}
      <ConfirmDialog
        open={discardAsk}
        onClose={() => setDiscardAsk(false)}
        tone="warn"
        title="放弃未保存的更改？"
        description={
          draft?.mode === "add" ? "已填写的新用户信息将不会被创建。" : `对 ${draft?.username ?? ""} 的修改尚未保存。`
        }
        consequences={["关闭后本次编辑内容将丢失"]}
        cancelLabel="继续编辑"
        confirmLabel="放弃更改"
        onConfirm={forceCloseDraft}
      />
    </section>
  );
}

/* ---------- small building blocks ---------- */

function FilterCount({ n }: { n: number }) {
  if (n <= 0) return null;
  return (
    <span className="rounded-full bg-line/70 px-1.5 text-[10px] leading-4 font-semibold text-ink-secondary tabular-nums">
      {n}
    </span>
  );
}

function StatTile({
  label,
  value,
  icon,
  tone = "default",
}: {
  label: string;
  value: React.ReactNode;
  icon: React.ReactNode;
  tone?: "default" | "accent" | "success" | "warn";
}) {
  return (
    <div className="rounded-xl border border-line bg-elevated px-3.5 py-2.5">
      <div
        className={cn(
          "flex items-center gap-1.5 text-[11px]",
          tone === "accent"
            ? "text-accent"
            : tone === "success"
              ? "text-success"
              : tone === "warn"
                ? "text-warn"
                : "text-ink-muted",
        )}
      >
        {icon}
        {label}
      </div>
      <p className="mt-1 font-serif text-lg leading-tight font-semibold text-ink">{value}</p>
    </div>
  );
}

function FormGroup({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2.5">
      <div className="flex items-baseline justify-between gap-2">
        <p className="text-[11px] font-semibold tracking-wider text-ink-muted uppercase">{title}</p>
        {hint ? <span className="text-[11px] text-ink-muted">{hint}</span> : null}
      </div>
      {children}
    </div>
  );
}

function ToggleRow({
  icon,
  title,
  titleClassName,
  desc,
  checked,
  disabled,
  onChange,
}: {
  icon?: React.ReactNode;
  title: string;
  titleClassName?: string;
  desc: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label
      className={cn(
        "flex items-center justify-between gap-3 px-4 py-3",
        disabled ? "cursor-not-allowed opacity-70" : "cursor-pointer",
      )}
    >
      <div className="flex min-w-0 items-start gap-2.5">
        {icon ? <span className="mt-0.5 shrink-0">{icon}</span> : null}
        <div className="min-w-0">
          <p className={cn("text-[13px] font-medium text-ink", titleClassName)}>{title}</p>
          <p className="text-[11px] text-ink-muted">{desc}</p>
        </div>
      </div>
      <Switch checked={checked} disabled={disabled} ariaLabel={title} onChange={onChange} />
    </label>
  );
}

function Callout({ tone, children }: { tone: "warn" | "danger"; children: React.ReactNode }) {
  return (
    <motion.div
      initial={{ opacity: 0, height: 0 }}
      animate={{ opacity: 1, height: "auto" }}
      exit={{ opacity: 0, height: 0 }}
      transition={SPRING_SNAPPY}
      className="overflow-hidden"
    >
      <div
        className={cn(
          "flex items-start gap-2 rounded-xl border px-3 py-2.5 text-[11.5px] leading-relaxed",
          tone === "danger"
            ? "border-danger/25 bg-danger-soft/60 text-danger"
            : "border-warn/25 bg-warn-soft/60 text-warn",
        )}
      >
        <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
        <span>{children}</span>
      </div>
    </motion.div>
  );
}

function QuotaField({
  label,
  icon,
  value,
  used,
  onChange,
}: {
  label: string;
  icon: React.ReactNode;
  value: number;
  used?: number;
  onChange: (v: number) => void;
}) {
  const belowUsage = used !== undefined && value > 0 && used > value;
  return (
    <div className="flex flex-col gap-2.5 rounded-2xl border border-line bg-elevated px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <span className="inline-flex items-center gap-1.5 text-[13px] font-medium text-ink">
          {icon}
          {label}
        </span>
        <input
          type="number"
          min={0}
          aria-label={label}
          value={String(value)}
          onChange={(e) => onChange(Math.max(0, Number(e.target.value) || 0))}
          className="h-8 w-24 rounded-lg border border-line-strong/80 bg-elevated px-2 text-right text-[13px] font-medium text-ink tabular-nums outline-none focus:border-accent/60 focus:ring-2 focus:ring-accent/15"
        />
      </div>
      <div className="flex flex-wrap items-center gap-1.5">
        {QUOTA_PRESETS.map((p) => (
          <button
            key={p}
            type="button"
            onClick={() => onChange(p)}
            className={cn(
              "inline-flex cursor-pointer items-center gap-1 rounded-full border px-2.5 py-0.5 text-[11px] tabular-nums transition-colors",
              value === p
                ? "border-accent bg-accent-soft text-accent"
                : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong",
            )}
          >
            {value === p ? <Check className="size-2.5" /> : null}
            {p === 0 ? "不限" : p}
          </button>
        ))}
      </div>
      {used !== undefined ? (
        <QuotaBar compact icon={null} label="当前用量" used={used} limit={value} />
      ) : null}
      {belowUsage ? (
        <p className="text-[11px] text-warn">
          新上限低于当前用量（{used}），已有数据不会被删除，但将无法继续新增。
        </p>
      ) : null}
    </div>
  );
}

function SaveState({ pending, dirty, mode }: { pending: boolean; dirty: boolean; mode: "add" | "edit" }) {
  if (pending) {
    return (
      <span className="inline-flex items-center gap-1.5 text-[11px] text-ink-muted">
        <span className="size-3 animate-spin rounded-full border-2 border-line-strong border-t-accent" />
        正在保存…
      </span>
    );
  }
  if (mode === "edit" && !dirty) {
    return (
      <span className="inline-flex items-center gap-1 text-[11px] text-ink-muted">
        <Check className="size-3 text-success" />
        无未保存更改
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1.5 text-[11px] font-medium text-warn">
      <span className="size-1.5 rounded-full bg-warn" />
      {mode === "add" ? "填写后点击创建" : "有未保存的更改"}
    </span>
  );
}

export default UsersSection;
