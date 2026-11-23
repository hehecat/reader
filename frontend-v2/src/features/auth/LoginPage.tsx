import { useQueryClient } from "@tanstack/react-query";
import {
  AlertCircle,
  BookOpen,
  Eye,
  EyeOff,
  KeyRound,
  Lock,
  User,
} from "lucide-react";
import * as React from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { ApiError } from "@/api/client";
import { authApi } from "@/api/endpoints";
import { cn } from "@/lib/cn";
import { PAPER_SWATCHES } from "@/shell/CommandPalette";
import { useAuthStore } from "@/stores/auth";
import { resolvePaperTheme, usePrefsStore } from "@/stores/prefs";
import { notify } from "@/stores/toast";
import { Button } from "@/ui/Button";
import { Segmented } from "@/ui/Controls";
import { Input } from "@/ui/Input";
import { AnimatePresence, EASE_PAPER, motion, SPRING_SILK, useReducedMotion } from "@/ui/motion";

type AuthMode = "login" | "register";

const AUTH_TABS = [
  { value: "login", label: "登录书房" },
  { value: "register", label: "初入注册" },
] as const;

function getSafeRedirect(raw: string | null): string {
  if (!raw) return "/";
  if (!raw.startsWith("/") || raw.startsWith("//")) return "/";
  if (raw.startsWith("/login")) return "/";
  return raw;
}

interface FieldErrors {
  username?: string;
  password?: string;
  code?: string;
  form?: string;
}

export default function LoginPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [searchParams] = useSearchParams();
  const reduced = useReducedMotion();

  const system = useAuthStore((s) => s.system);
  const setSession = useAuthStore((s) => s.setSession);
  const theme = usePrefsStore((s) => s.theme);
  const setTheme = usePrefsStore((s) => s.setTheme);
  const resolvedTheme = resolvePaperTheme(theme);

  const [mode, setMode] = React.useState<AuthMode>("login");
  const [username, setUsername] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [showPassword, setShowPassword] = React.useState(false);
  const [inviteCode, setInviteCode] = React.useState("");
  const [errors, setErrors] = React.useState<FieldErrors>({});
  const [submitting, setSubmitting] = React.useState(false);
  const [shaking, setShaking] = React.useState(false);

  const triggerShake = React.useCallback(() => {
    setShaking(false);
    window.requestAnimationFrame(() => setShaking(true));
  }, []);

  const usernameRef = React.useRef<HTMLInputElement>(null);
  const passwordRef = React.useRef<HTMLInputElement>(null);
  const inviteRef = React.useRef<HTMLInputElement>(null);

  const inviteRequired = Boolean(system?.inviteRequired);
  const isRegister = mode === "register";

  const handleModeChange = (next: AuthMode) => {
    setMode(next);
    setErrors({});
    setShowPassword(false);
  };

  const validate = (): FieldErrors => {
    const next: FieldErrors = {};
    const trimmed = username.trim();

    if (!trimmed) {
      next.username = "请输入用户名";
    } else if (isRegister) {
      if (trimmed.length < 5) {
        next.username = "用户名至少 5 位字符";
      } else if (!/^[a-zA-Z0-9]+$/.test(trimmed)) {
        next.username = "仅支持英文字母与数字";
      } else if (trimmed.toLowerCase() === "default") {
        next.username = "该用户名不可使用";
      }
    }

    if (!password) {
      next.password = "请输入密码";
    }

    if (isRegister && inviteRequired && !inviteCode.trim()) {
      next.code = "请输入邀请码";
    }

    return next;
  };

  const focusFirstError = (fieldErrors: FieldErrors) => {
    if (fieldErrors.username) usernameRef.current?.focus();
    else if (fieldErrors.password) passwordRef.current?.focus();
    else if (fieldErrors.code) inviteRef.current?.focus();
  };

  const handleSubmit = async (e: React.FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    if (submitting) return;

    const fieldErrors = validate();
    if (Object.keys(fieldErrors).length > 0) {
      setErrors(fieldErrors);
      triggerShake();
      focusFirstError(fieldErrors);
      return;
    }

    setErrors({});
    setSubmitting(true);

    try {
      const user = await authApi.login({
        username: username.trim(),
        password,
        isLogin: !isRegister,
        code: isRegister && inviteRequired ? inviteCode.trim() : undefined,
      });

      queryClient.clear();
      setSession(user);
      // 刷新系统信息以同步管理员状态并下发 EPUB 鉴权 Cookie（避免覆盖已建立会话）
      void authApi
        .getSystemInfo()
        .then((sys) => {
          useAuthStore.setState({ system: sys });
        })
        .catch(() => undefined);

      notify.success(isRegister ? "注册成功，欢迎进入书房" : "欢迎回到书房");
      const target = getSafeRedirect(searchParams.get("redirect"));
      navigate(target, { replace: true });
    } catch (err) {
      const message =
        err instanceof ApiError
          ? err.message
          : err instanceof Error
            ? err.message
            : isRegister
              ? "注册失败，请检查输入信息"
              : "登录失败，请检查用户名或密码";
      setErrors({ form: message });
      triggerShake();
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="paper-grain relative flex min-h-dvh flex-col items-center justify-center overflow-hidden bg-bg px-4 py-12 text-ink">
      {/* Subtle archival ambient wash & book-margin hairlines */}
      <div aria-hidden className="pointer-events-none fixed inset-0 overflow-hidden">
        <div
          className="absolute top-[-16%] left-1/2 h-[440px] w-[660px] -translate-x-1/2 rounded-full opacity-40 blur-3xl dark:opacity-25"
          style={{
            background: "radial-gradient(closest-side, var(--accent-soft), transparent 72%)",
          }}
        />
        <div
          className="absolute right-[-10%] bottom-[-20%] h-[360px] w-[480px] rounded-full opacity-25 blur-3xl dark:opacity-15"
          style={{
            background: "radial-gradient(closest-side, var(--accent-soft), transparent 75%)",
          }}
        />
        {/* 典雅竖排题词水印（桌面宽屏呈现） */}
        <p className="vertical-rl fixed top-14 right-10 hidden font-serif text-xs tracking-[0.42em] text-ink-muted/35 select-none lg:block">
          展卷墨香静 · 伏案读书声
        </p>
      </div>

      {/* 顶部纸色快切胶囊：登录前即可预览四套东方纸墨 */}
      <div className="safe-t fixed top-4 right-4 z-20 sm:top-5 sm:right-6">
        <div
          role="group"
          aria-label="切换纸张色调"
          className="surface-glass flex items-center gap-1.5 rounded-full px-2 py-1.5"
        >
          {PAPER_SWATCHES.map((sw) => {
            const active = resolvedTheme === sw.id;
            return (
              <button
                key={sw.id}
                type="button"
                title={sw.name}
                aria-label={`切换至${sw.name}`}
                aria-pressed={active}
                onClick={() => setTheme(sw.id)}
                style={{
                  backgroundColor: sw.bg,
                  borderColor: active ? sw.accent : sw.border,
                }}
                className={cn(
                  "flex size-5 cursor-pointer items-center justify-center rounded-full border transition-transform duration-150 hover:scale-110 active:scale-95",
                  active && "ring-2 ring-accent/40 ring-offset-1 ring-offset-elevated",
                )}
              >
                <span
                  style={{ backgroundColor: sw.accent }}
                  className={cn("rounded-full transition-all", active ? "size-2" : "size-1 opacity-70")}
                />
              </button>
            );
          })}
        </div>
      </div>

      {/* Centered Editorial Glass Sanctuary Card */}
      <motion.div
        initial={reduced ? false : { opacity: 0, y: 16, scale: 0.985 }}
        animate={{ opacity: 1, y: 0, scale: 1 }}
        transition={reduced ? { duration: 0.12 } : SPRING_SILK}
        className="relative z-10 w-full max-w-[404px]"
      >
        <div
          onAnimationEnd={() => setShaking(false)}
          className={cn(
            "surface-glass relative overflow-hidden rounded-3xl p-7 ring-1 ring-inset ring-white/45 sm:p-9 dark:ring-white/8",
            shaking && "shake-x",
          )}
        >
          {/* 顶部书卷朱砂细装饰线 */}
          <span
            aria-hidden
            className="pointer-events-none absolute inset-x-10 top-0 h-[2px] rounded-b-full opacity-65"
            style={{
              background:
                "linear-gradient(90deg, transparent, var(--accent), transparent)",
            }}
          />

          {/* Brand Badge & Editorial Welcome */}
          <div className="flex flex-col items-center text-center">
            <div className="flex size-12 items-center justify-center rounded-2xl bg-accent text-accent-ink shadow-[var(--shadow-primary)]">
              <BookOpen className="size-5" aria-hidden />
            </div>
            <h1 className="mt-3.5 font-serif text-[23px] leading-tight font-semibold tracking-[0.12em] text-ink">
              砚台
            </h1>
            <p className="mt-1 text-[10px] font-medium tracking-[0.26em] text-ink-muted">
              READER DESK
            </p>
            <p className="mt-3 text-xs leading-relaxed text-ink-secondary">
              {isRegister
                ? "创建专属书房，同步藏书进度与阅读笔记"
                : "一卷在手，静享清欢 · 登录你的私人书房"}
            </p>
          </div>

          {/* Auth Mode Switcher */}
          <div className="mt-6">
            <Segmented<AuthMode>
              layoutId="v2-auth-tab"
              ariaLabel="登录或注册"
              fullWidth
              options={AUTH_TABS}
              value={mode}
              onChange={handleModeChange}
            />
          </div>

          {/* Form */}
          <form onSubmit={(e) => void handleSubmit(e)} noValidate className="mt-6 space-y-4">
            <Input
              ref={usernameRef}
              label="用户名"
              size="lg"
              value={username}
              onChange={(e) => {
                setUsername(e.target.value);
                if (errors.username || errors.form) {
                  setErrors((prev) => ({ ...prev, username: undefined, form: undefined }));
                }
              }}
              placeholder={isRegister ? "至少 5 位英文字母或数字" : "输入用户名"}
              hint={isRegister && !errors.username ? "仅支持英文字母与数字，长度不少于 5 位" : undefined}
              autoComplete="username"
              autoCapitalize="none"
              spellCheck={false}
              clearable
              onClear={() => {
                setUsername("");
                usernameRef.current?.focus();
              }}
              leftIcon={<User className="size-4" />}
              error={errors.username}
            />

            <Input
              ref={passwordRef}
              label="密码"
              size="lg"
              type={showPassword ? "text" : "password"}
              value={password}
              onChange={(e) => {
                setPassword(e.target.value);
                if (errors.password || errors.form) {
                  setErrors((prev) => ({ ...prev, password: undefined, form: undefined }));
                }
              }}
              placeholder={isRegister ? "设置登录密码" : "输入密码"}
              autoComplete={isRegister ? "new-password" : "current-password"}
              leftIcon={<Lock className="size-4" />}
              rightSlot={
                <button
                  type="button"
                  onClick={() => setShowPassword((prev) => !prev)}
                  aria-label={showPassword ? "隐藏密码" : "显示密码"}
                  tabIndex={-1}
                  className="flex size-7 cursor-pointer items-center justify-center rounded-lg text-ink-muted transition-colors hover:bg-subtle hover:text-ink"
                >
                  {showPassword ? <EyeOff className="size-4" /> : <Eye className="size-4" />}
                </button>
              }
              error={errors.password}
            />

            <AnimatePresence initial={false}>
              {isRegister && inviteRequired ? (
                <motion.div
                  key="invite-code"
                  initial={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                  animate={{ opacity: 1, height: "auto" }}
                  exit={reduced ? { opacity: 0 } : { opacity: 0, height: 0 }}
                  transition={{ duration: 0.22, ease: EASE_PAPER }}
                  className="overflow-hidden"
                >
                  <div className="pt-0.5">
                    <Input
                      ref={inviteRef}
                      label="邀请码"
                      size="lg"
                      value={inviteCode}
                      onChange={(e) => {
                        setInviteCode(e.target.value);
                        if (errors.code || errors.form) {
                          setErrors((prev) => ({ ...prev, code: undefined, form: undefined }));
                        }
                      }}
                      placeholder="输入管理员提供的邀请码"
                      autoComplete="off"
                      leftIcon={<KeyRound className="size-4" />}
                      error={errors.code}
                    />
                  </div>
                </motion.div>
              ) : null}
            </AnimatePresence>

            <AnimatePresence initial={false}>
              {errors.form ? (
                <motion.div
                  key="form-error"
                  initial={{ opacity: 0, y: -4 }}
                  animate={{ opacity: 1, y: 0 }}
                  exit={{ opacity: 0, y: -4 }}
                  transition={{ duration: 0.18, ease: EASE_PAPER }}
                  role="alert"
                  className="flex items-start gap-2 rounded-xl border border-danger/25 bg-danger-soft/85 px-3.5 py-2.5 text-xs leading-relaxed text-danger"
                >
                  <AlertCircle className="mt-0.5 size-3.5 shrink-0" aria-hidden />
                  <span>{errors.form}</span>
                </motion.div>
              ) : null}
            </AnimatePresence>

            <div className="pt-1.5">
              <Button
                type="submit"
                variant="primary"
                size="lg"
                loading={submitting}
                className="h-11 w-full text-[14px] tracking-wide"
              >
                {submitting
                  ? isRegister
                    ? "正在创建书房…"
                    : "正在推开书房…"
                  : isRegister
                    ? "创建书房账号"
                    : "进入书房"}
              </Button>
            </div>
          </form>
        </div>

        {/* Archival footer colophon */}
        <p className="mt-5 text-center text-[11px] tracking-[0.12em] text-ink-muted/75 select-none">
          砚台私有书房{system?.version ? ` · v${system.version}` : ""}
        </p>
      </motion.div>
    </div>
  );
}
