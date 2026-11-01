import { BookOpen, Gift, Lock, User } from "lucide-react";
import { useState, type FormEvent } from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { Button, Input, Tabs, TabsList, TabsTrigger, toast } from "@/components/ui";
import { resolveRedirect } from "@/hooks/useAuth";
import { login } from "@/services/auth";
import { useAuthStore } from "@/stores/auth-store";

type AuthTab = "login" | "register";

/** 后端注册校验: 用户名 ≥5 位, 密码 ≥8 位(UserController), 提前拦截省一次请求 */
const REGISTER_MIN_USERNAME = 5;
const REGISTER_MIN_PASSWORD = 8;

/** 登录 / 注册页: 居中卡片, tab 切换 isLogin, 注册时多一个邀请码输入. */
export default function LoginPage() {
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const [tab, setTab] = useState<AuthTab>("login");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [pending, setPending] = useState(false);

  const isLogin = tab === "login";

  const handleSubmit = async (event: FormEvent<HTMLFormElement>): Promise<void> => {
    event.preventDefault();
    if (pending) {
      return;
    }
    const name = username.trim();
    if (name.length === 0 || password.length === 0) {
      toast.error("请输入用户名和密码");
      return;
    }
    if (!isLogin) {
      if (name.length < REGISTER_MIN_USERNAME) {
        toast.error(`用户名不能低于${String(REGISTER_MIN_USERNAME)}位`);
        return;
      }
      if (password.length < REGISTER_MIN_PASSWORD) {
        toast.error(`密码不能低于${String(REGISTER_MIN_PASSWORD)}位`);
        return;
      }
    }
    setPending(true);
    try {
      const invite = isLogin ? undefined : code.trim() || undefined;
      const result = await login(name, password, isLogin, invite);
      useAuthStore.getState().login(result);
      toast.success(isLogin ? "登录成功" : "注册成功");
      navigate(resolveRedirect(searchParams.get("redirect")), { replace: true });
    } catch (error) {
      // ApiError.message 即后端 errorMsg, 已区分场景:
      // 登录 → "用户不存在"/"密码错误", 注册 → "用户名已被占用"/"用户名不能低于5位" 等
      const message = error instanceof Error && error.message.length > 0 ? error.message : "";
      toast.error(message.length > 0 ? message : isLogin ? "登录失败" : "注册失败");
    } finally {
      setPending(false);
    }
  };

  return (
    <div className="flex min-h-full items-center justify-center bg-background px-4 py-10">
      <div className="w-full max-w-sm">
        <div className="mb-6 flex flex-col items-center gap-2">
          <span className="flex size-12 items-center justify-center rounded-2xl bg-accent text-accent-foreground shadow-sm">
            <BookOpen className="size-6" aria-hidden />
          </span>
          <h1 className="text-2xl font-semibold tracking-tight">阅读</h1>
          <p className="text-sm text-muted-foreground">登录后同步书架与阅读进度</p>
        </div>

        <div className="rounded-2xl border bg-surface p-6 shadow-sm">
          <Tabs
            value={tab}
            onValueChange={(value) => {
              setTab(value === "register" ? "register" : "login");
            }}
          >
            <TabsList className="w-full">
              <TabsTrigger value="login" className="flex-1">
                登录
              </TabsTrigger>
              <TabsTrigger value="register" className="flex-1">
                注册
              </TabsTrigger>
            </TabsList>
          </Tabs>

          <form
            className="mt-5 space-y-4"
            onSubmit={(event) => {
              void handleSubmit(event);
            }}
          >
            <div>
              <label htmlFor="login-username" className="mb-1.5 block text-xs text-muted-foreground">
                用户名
              </label>
              <Input
                id="login-username"
                prefixIcon={<User className="size-4" aria-hidden />}
                placeholder="用户名"
                autoComplete="username"
                autoFocus
                value={username}
                onChange={(event) => setUsername(event.target.value)}
                disabled={pending}
              />
            </div>
            <div>
              <label htmlFor="login-password" className="mb-1.5 block text-xs text-muted-foreground">
                {isLogin ? "密码" : "密码(至少 8 位)"}
              </label>
              <Input
                id="login-password"
                type="password"
                prefixIcon={<Lock className="size-4" aria-hidden />}
                placeholder={isLogin ? "密码" : "密码(至少 8 位)"}
                autoComplete={isLogin ? "current-password" : "new-password"}
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                disabled={pending}
              />
            </div>
            {isLogin ? null : (
              <div>
                <label htmlFor="login-invite" className="mb-1.5 block text-xs text-muted-foreground">
                  邀请码(没有则不填)
                </label>
                <Input
                  id="login-invite"
                  prefixIcon={<Gift className="size-4" aria-hidden />}
                  placeholder="邀请码(没有则不填)"
                  autoComplete="off"
                  value={code}
                  onChange={(event) => setCode(event.target.value)}
                  disabled={pending}
                />
              </div>
            )}
            <Button type="submit" size="lg" className="w-full" loading={pending}>
              {isLogin ? "登录" : "注册"}
            </Button>
          </form>
        </div>
      </div>
    </div>
  );
}
