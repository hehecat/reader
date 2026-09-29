import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  KeyRound,
  Pencil,
  Plus,
  RefreshCw,
  ShieldCheck,
  Trash2,
  UserMinus,
  UserCheck,
} from "lucide-react";
import * as React from "react";

import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  IconButton,
  Input,
  PageIntro,
  SettingCard,
  Switch,
  cn,
  toast,
} from "@/components/ui";
import { errorMessage } from "@/hooks/useBookshelf";
import { useAppMode } from "@/hooks/useAppMode";
import { humanizeError } from "@/lib/errors";
import {
  addUser,
  clearInactiveUsers,
  deleteUser,
  listUsers,
  resetUserPassword,
  updateUser,
  type AdminUser,
} from "@/services/users";

const USERS_QUERY_KEY = ["adminUsers"];

const dayFormatter = new Intl.DateTimeFormat("zh-CN", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
});

function formatTime(value: number): string {
  return value > 0 ? dayFormatter.format(value) : "—";
}

/** 权限开关表: 与后端 User 字段一一对应(用户管理页逐项可调) */
const PERMISSIONS = [
  { key: "enableBookSource", label: "书源" },
  { key: "enableRssSource", label: "订阅源" },
  { key: "enableLocalStore", label: "本地存储" },
  { key: "enableWebdav", label: "WebDAV 备份" },
] as const;

type PermissionKey = (typeof PERMISSIONS)[number]["key"];

interface UserDraft {
  username: string;
  password: string;
  isAdmin: boolean;
  disabled: boolean;
  enableBookSource: boolean;
  enableRssSource: boolean;
  enableLocalStore: boolean;
  enableWebdav: boolean;
  bookSourceLimit: number;
  bookLimit: number;
}

const EMPTY_DRAFT: UserDraft = {
  username: "",
  password: "",
  isAdmin: false,
  disabled: false,
  enableBookSource: true,
  enableRssSource: true,
  enableLocalStore: false,
  enableWebdav: false,
  bookSourceLimit: 80000,
  bookLimit: 5000,
};

function draftFrom(user: AdminUser): UserDraft {
  return {
    username: user.username,
    password: "",
    isAdmin: user.isAdmin,
    disabled: user.disabled,
    enableBookSource: user.enableBookSource,
    enableRssSource: user.enableRssSource,
    enableLocalStore: user.enableLocalStore,
    enableWebdav: user.enableWebdav,
    bookSourceLimit: user.bookSourceLimit,
    bookLimit: user.bookLimit,
  };
}

/**
 * 用户管理页(仅管理员): 列表 + 新建/编辑权限配额 + 启停用 + 改密 + 删除 + 清理不活跃.
 * 后端接口: getUsers/addUser/updateUser/deleteUser/resetUserPassword/clearInactiveUsers
 * (多用户模式要求已登录且 is_admin; 单用户模式无登录概念).
 */
export default function UsersPage() {
  const queryClient = useQueryClient();
  const { isAdmin } = useAppMode();
  const [editing, setEditing] = React.useState<{ mode: "create" | "edit"; draft: UserDraft } | null>(
    null,
  );
  const [passwordTarget, setPasswordTarget] = React.useState<AdminUser | null>(null);
  const [newPassword, setNewPassword] = React.useState("");
  const [inactiveDays, setInactiveDays] = React.useState("90");

  const users = useQuery({
    queryKey: USERS_QUERY_KEY,
    queryFn: () => listUsers(),
    enabled: isAdmin,
    retry: false,
  });

  const invalidate = React.useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: USERS_QUERY_KEY });
  }, [queryClient]);

  const onError = React.useCallback((fallback: string) => {
    return (error: unknown) => {
      toast.error(humanizeError(errorMessage(error, fallback)));
    };
  }, []);

  const saveUser = useMutation({
    mutationFn: async (input: UserDraft) => {
      if (editing?.mode === "create") {
        await addUser({
          username: input.username,
          password: input.password,
          isAdmin: input.isAdmin,
          enableBookSource: input.enableBookSource,
          enableRssSource: input.enableRssSource,
          enableLocalStore: input.enableLocalStore,
          enableWebdav: input.enableWebdav,
          bookSourceLimit: input.bookSourceLimit,
          bookLimit: input.bookLimit,
        });
        return;
      }
      await updateUser({
        username: input.username,
        isAdmin: input.isAdmin,
        disabled: input.disabled,
        enableBookSource: input.enableBookSource,
        enableRssSource: input.enableRssSource,
        enableLocalStore: input.enableLocalStore,
        enableWebdav: input.enableWebdav,
        bookSourceLimit: input.bookSourceLimit,
        bookLimit: input.bookLimit,
      });
    },
    onSuccess: () => {
      toast.success(editing?.mode === "create" ? "用户已创建" : "已保存");
      setEditing(null);
      invalidate();
    },
    onError: onError("保存失败"),
  });

  const toggleDisabled = useMutation({
    mutationFn: (user: AdminUser) =>
      updateUser({ username: user.username, disabled: !user.disabled }),
    onSuccess: (_r, user) => {
      toast.success(user.disabled ? `已启用 ${user.username}` : `已停用 ${user.username}`);
      invalidate();
    },
    onError: onError("操作失败"),
  });

  const removeUser = useMutation({
    mutationFn: (user: AdminUser) => deleteUser(user.username),
    onSuccess: () => {
      toast.success("用户已删除");
      invalidate();
    },
    onError: onError("删除失败"),
  });

  const savePassword = useMutation({
    mutationFn: (input: { user: AdminUser; password: string }) =>
      resetUserPassword(input.user.username, input.password),
    onSuccess: () => {
      toast.success("密码已重置");
      setPasswordTarget(null);
      setNewPassword("");
    },
    onError: onError("重置密码失败"),
  });

  const cleanInactive = useMutation({
    mutationFn: (days: number) => clearInactiveUsers(days),
    onSuccess: (count) => {
      toast.success(count > 0 ? `已清理 ${count} 个不活跃用户` : "没有可清理的用户");
      invalidate();
    },
    onError: onError("清理失败"),
  });

  // 页面容器与站内其它页一致: 居中限宽 + 左右留白
  const pageClass =
    "mx-auto flex min-h-full w-full max-w-5xl flex-col px-4 pb-10 pt-5 sm:px-6 md:px-10 md:pt-8";

  if (!isAdmin) {
    return (
      <div className={pageClass}>
        <PageIntro eyebrow="SERVER ADMIN" title="用户管理" desc="仅管理员可访问" />
        <EmptyState
          icon={<ShieldCheck aria-hidden />}
          title="需要管理员身份"
          description="当前账号不是管理员, 无法管理用户。请联系服务器管理员。"
        />
      </div>
    );
  }

  const list = users.data ?? [];

  return (
    <div className={pageClass}>
      <PageIntro
        eyebrow="SERVER ADMIN"
        title="用户管理"
        desc="账号 · 权限 · 配额; 停用后立即失效(含已登录 token)"
        action={
          <div className="flex items-center gap-2">
            <IconButton
              variant="ghost"
              aria-label="刷新用户列表"
              tooltip="刷新"
              disabled={users.isFetching}
              onClick={() => void users.refetch()}
            >
              <RefreshCw aria-hidden className={cn(users.isFetching && "ui-spin")} />
            </IconButton>
            <Button
              size="sm"
              onClick={() => setEditing({ mode: "create", draft: { ...EMPTY_DRAFT } })}
            >
              <Plus aria-hidden />
              新建用户
            </Button>
          </div>
        }
      />

      <div className="flex flex-col gap-5">
        <SettingCard title="账号" desc={`共 ${list.length} 个用户`}>
          {users.isLoading ? (
            <p className="py-6 text-center text-sm text-muted-foreground">正在读取用户…</p>
          ) : users.isError ? (
            <p className="py-6 text-center text-sm text-danger">
              {humanizeError(errorMessage(users.error, "读取用户失败"))}
            </p>
          ) : list.length === 0 ? (
            <p className="py-6 text-center text-sm text-muted-foreground">暂无用户</p>
          ) : (
            <ul className="divide-y divide-border/70">
              {list.map((user) => (
                <li
                  key={user.username}
                  className="flex flex-col gap-3 py-4 first:pt-0 last:pb-0 sm:flex-row sm:items-center sm:justify-between"
                >
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="truncate font-medium">{user.username}</span>
                      {user.isAdmin ? <Badge variant="accent">管理员</Badge> : null}
                      {user.disabled ? <Badge variant="muted">已停用</Badge> : null}
                    </div>
                    <p className="mt-1 text-xs text-muted-foreground">
                      {PERMISSIONS.filter((p) => user[p.key]).map((p) => p.label).join(" · ") ||
                        "无功能权限"}
                      {" · "}
                      书源 {user.bookSourceLimit} / 书 {user.bookLimit}
                      {" · "}
                      注册 {formatTime(user.createdAt)} · 上次登录 {formatTime(user.lastLoginAt)}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    <IconButton
                      size="sm"
                      variant="ghost"
                      aria-label={`编辑 ${user.username}`}
                      tooltip="编辑权限/配额"
                      onClick={() => setEditing({ mode: "edit", draft: draftFrom(user) })}
                    >
                      <Pencil aria-hidden />
                    </IconButton>
                    <IconButton
                      size="sm"
                      variant="ghost"
                      aria-label={`重置 ${user.username} 的密码`}
                      tooltip="重置密码"
                      onClick={() => {
                        setNewPassword("");
                        setPasswordTarget(user);
                      }}
                    >
                      <KeyRound aria-hidden />
                    </IconButton>
                    <IconButton
                      size="sm"
                      variant="ghost"
                      aria-label={user.disabled ? `启用 ${user.username}` : `停用 ${user.username}`}
                      tooltip={user.disabled ? "启用" : "停用(立即失效)"}
                      loading={toggleDisabled.isPending && toggleDisabled.variables?.username === user.username}
                      onClick={() => toggleDisabled.mutate(user)}
                    >
                      {user.disabled ? <UserCheck aria-hidden /> : <UserMinus aria-hidden />}
                    </IconButton>
                    <IconButton
                      size="sm"
                      variant="ghost"
                      aria-label={`删除 ${user.username}`}
                      tooltip="删除用户"
                      onClick={() => {
                        if (window.confirm(`删除用户 ${user.username}? 其书架/书源/进度将一并删除。`)) {
                          removeUser.mutate(user);
                        }
                      }}
                    >
                      <Trash2 aria-hidden />
                    </IconButton>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </SettingCard>

        <SettingCard title="维护" desc="清理长期未登录的账号(慎用, 不可恢复)">
          <div className="flex flex-col gap-3 py-4 sm:flex-row sm:items-center">
            <Input
              aria-label="不活跃天数"
              inputMode="numeric"
              className="sm:max-w-32"
              value={inactiveDays}
              onChange={(event) => setInactiveDays(event.target.value.replace(/\D/g, ""))}
            />
            <span className="text-xs text-muted-foreground">天未登录的用户将被删除</span>
            <Button
              size="sm"
              variant="danger"
              loading={cleanInactive.isPending}
              disabled={inactiveDays === "" || Number(inactiveDays) <= 0}
              onClick={() => {
                const days = Number(inactiveDays);
                if (window.confirm(`删除 ${days} 天未登录的用户? 不可恢复。`)) {
                  cleanInactive.mutate(days);
                }
              }}
            >
              清理不活跃用户
            </Button>
          </div>
        </SettingCard>
      </div>

      {/* 新建 / 编辑 */}
      <Dialog open={editing !== null} onOpenChange={(open) => (open ? undefined : setEditing(null))}>
        <DialogContent width="md">
          <DialogHeader>
            <DialogTitle>{editing?.mode === "create" ? "新建用户" : "编辑用户"}</DialogTitle>
            <DialogDescription>
              {editing?.mode === "create"
                ? "创建后可用该账号登录; 权限与配额可随时调整"
                : "停用后该账号无法登录, 已登录会话立即失效"}
            </DialogDescription>
          </DialogHeader>
          {editing ? (
            <div className="flex flex-col gap-4 px-4 md:px-5">
              <label className="flex flex-col gap-1.5 text-sm">
                <span className="text-muted-foreground">用户名</span>
                <Input
                  aria-label="用户名"
                  disabled={editing.mode === "edit"}
                  value={editing.draft.username}
                  onChange={(event) =>
                    setEditing({ ...editing, draft: { ...editing.draft, username: event.target.value } })
                  }
                />
              </label>
              {editing.mode === "create" ? (
                <label className="flex flex-col gap-1.5 text-sm">
                  <span className="text-muted-foreground">初始密码(不少于 6 位)</span>
                  <Input
                    type="password"
                    aria-label="初始密码"
                    value={editing.draft.password}
                    onChange={(event) =>
                      setEditing({ ...editing, draft: { ...editing.draft, password: event.target.value } })
                    }
                  />
                </label>
              ) : null}
              <div className="grid grid-cols-2 gap-3">
                {PERMISSIONS.map((item) => (
                  <div key={item.key} className="flex items-center justify-between gap-2 text-sm">
                    <span>{item.label}</span>
                    <Switch
                      aria-label={item.label}
                      checked={editing.draft[item.key as PermissionKey]}
                      onCheckedChange={(checked) =>
                        setEditing({
                          ...editing,
                          draft: { ...editing.draft, [item.key]: checked } as UserDraft,
                        })
                      }
                    />
                  </div>
                ))}
                <div className="flex items-center justify-between gap-2 text-sm">
                  <span>管理员</span>
                  <Switch
                    aria-label="管理员"
                    checked={editing.draft.isAdmin}
                    onCheckedChange={(checked) =>
                      setEditing({ ...editing, draft: { ...editing.draft, isAdmin: checked } })
                    }
                  />
                </div>
                {editing.mode === "edit" ? (
                  <div className="flex items-center justify-between gap-2 text-sm">
                    <span>停用账号</span>
                    <Switch
                      aria-label="停用账号"
                      checked={editing.draft.disabled}
                      onCheckedChange={(checked) =>
                        setEditing({ ...editing, draft: { ...editing.draft, disabled: checked } })
                      }
                    />
                  </div>
                ) : null}
              </div>
              <div className="grid grid-cols-2 gap-3">
                <label className="flex flex-col gap-1.5 text-sm">
                  <span className="text-muted-foreground">书源上限</span>
                  <Input
                    aria-label="书源上限"
                    inputMode="numeric"
                    value={String(editing.draft.bookSourceLimit)}
                    onChange={(event) =>
                      setEditing({
                        ...editing,
                        draft: {
                          ...editing.draft,
                          bookSourceLimit: Number(event.target.value.replace(/\D/g, "") || 0),
                        },
                      })
                    }
                  />
                </label>
                <label className="flex flex-col gap-1.5 text-sm">
                  <span className="text-muted-foreground">书籍上限</span>
                  <Input
                    aria-label="书籍上限"
                    inputMode="numeric"
                    value={String(editing.draft.bookLimit)}
                    onChange={(event) =>
                      setEditing({
                        ...editing,
                        draft: {
                          ...editing.draft,
                          bookLimit: Number(event.target.value.replace(/\D/g, "") || 0),
                        },
                      })
                    }
                  />
                </label>
              </div>
            </div>
          ) : null}
          <DialogFooter>
            <Button variant="ghost" onClick={() => setEditing(null)}>
              取消
            </Button>
            <Button
              loading={saveUser.isPending}
              disabled={
                editing === null ||
                editing.draft.username.trim() === "" ||
                (editing.mode === "create" && editing.draft.password.length < 6)
              }
              onClick={() => {
                if (editing) {
                  saveUser.mutate(editing.draft);
                }
              }}
            >
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 重置密码 */}
      <Dialog
        open={passwordTarget !== null}
        onOpenChange={(open) => (open ? undefined : setPasswordTarget(null))}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>重置密码</DialogTitle>
            <DialogDescription>{passwordTarget?.username}</DialogDescription>
          </DialogHeader>
          <div className="px-4 md:px-5">
            <Input
              type="password"
              aria-label="新密码"
              placeholder="新密码(不少于 6 位)"
              value={newPassword}
              onChange={(event) => setNewPassword(event.target.value)}
            />
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setPasswordTarget(null)}>
              取消
            </Button>
            <Button
              loading={savePassword.isPending}
              disabled={newPassword.length < 6 || passwordTarget === null}
              onClick={() => {
                if (passwordTarget) {
                  savePassword.mutate({ user: passwordTarget, password: newPassword });
                }
              }}
            >
              重置
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
