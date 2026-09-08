import { KeyRound, Cookie } from "lucide-react";
import * as React from "react";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Input,
  Spinner,
  toast,
} from "@/components/ui";
import {
  loginBookSource,
  setBookSourceCookie,
  type SourceLoginResult,
} from "@/services/sources";
import type { BookSource } from "@/types/api";

export interface SourceLoginDialogProps {
  source: BookSource | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

interface LoginField {
  name: string;
  label: string;
  password: boolean;
}

/** legado loginUi: [{"name":"username","type":"input","text":{"label":"账号","isPassword":false}}] */
function parseLoginUi(source: BookSource): LoginField[] {
  try {
    const raw = source.loginUi ? (JSON.parse(source.loginUi) as unknown[]) : [];
    if (Array.isArray(raw) && raw.length > 0) {
      return raw
        .map((item) => {
          const it = (item ?? {}) as Record<string, unknown>;
          const text = (it.text ?? {}) as Record<string, unknown>;
          const name = String(it.name ?? "");
          const label = String(text.label ?? name);
          const password =
            it.type === "password" ||
            text.isPassword === true ||
            /pass|pwd/i.test(name);
          return { name, label, password };
        })
        .filter((f) => f.name.length > 0);
    }
  } catch {
    // loginUi 损坏 → 回退默认表单
  }
  return [
    { name: "username", label: "账号", password: false },
    { name: "password", label: "密码", password: true },
  ];
}

/** 书源登录: 表单登录 → 图片验证码重试 → 手动 Cookie 兜底 (warp loginBookSource 契约) */
export function SourceLoginDialog({ source, open, onOpenChange }: SourceLoginDialogProps) {
  const fields = React.useMemo(() => (source ? parseLoginUi(source) : []), [source]);
  const [values, setValues] = React.useState<Record<string, string>>({});
  const [busy, setBusy] = React.useState(false);
  const [captcha, setCaptcha] = React.useState<SourceLoginResult | null>(null);
  const [captchaText, setCaptchaText] = React.useState("");
  const [notice, setNotice] = React.useState<string | null>(null);
  const [cookieMode, setCookieMode] = React.useState(false);
  const [cookieText, setCookieText] = React.useState("");

  React.useEffect(() => {
    if (open) {
      setValues({});
      setCaptcha(null);
      setCaptchaText("");
      setNotice(null);
      setCookieMode(false);
      setCookieText("");
    }
  }, [open, source]);

  if (!source) return null;

  const submit = async (withCaptcha?: string) => {
    setBusy(true);
    setNotice(null);
    try {
      const res = await loginBookSource({
        bookSource: source.bookSourceUrl,
        username: values.username ?? "",
        password: values.password ?? "",
        ...Object.fromEntries(
          fields
            .filter((f) => f.name !== "username" && f.name !== "password")
            .map((f) => [f.name, values[f.name] ?? ""]),
        ),
        captcha: withCaptcha ?? (captchaText || undefined),
        captchaId: captcha?.captchaId,
      });
      if (res.success) {
        toast.success("登录成功, Cookie 已保存");
        onOpenChange(false);
      } else if (res.needCaptcha) {
        setCaptcha(res);
        setNotice(res.message || null);
      } else if (res.needManualCaptcha) {
        setCaptcha(null);
        setCookieMode(true);
        setNotice(res.message || "自动登录不可用, 请手动粘贴 Cookie");
      } else {
        setCaptcha(null);
        setNotice(res.message || "登录失败");
      }
    } catch (e) {
      setNotice(e instanceof Error ? e.message : "登录请求失败");
    } finally {
      setBusy(false);
    }
  };

  const saveCookie = async () => {
    setBusy(true);
    try {
      await setBookSourceCookie(source.bookSourceUrl, cookieText.trim());
      toast.success(cookieText.trim() ? "Cookie 已保存" : "Cookie 已清除");
      onOpenChange(false);
    } catch (e) {
      setNotice(e instanceof Error ? e.message : "保存失败");
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <KeyRound aria-hidden className="size-4" />
            登录书源 · {source.bookSourceName}
          </DialogTitle>
          <DialogDescription>
            登录态(Cookie)按当前账号保存, 仅用于该书源的搜索/目录/正文请求
          </DialogDescription>
        </DialogHeader>

        {cookieMode ? (
          <div className="space-y-3 px-4 pb-4 md:px-5 md:pb-5">
            <p className="text-sm text-muted-foreground">
              在浏览器登录该书源网站后, 复制请求头里的 Cookie 串粘贴到下方
            </p>
            <textarea
              className="border-input bg-background min-h-24 w-full rounded-md border px-3 py-2 font-mono text-xs"
              placeholder="name=value; name2=value2"
              value={cookieText}
              onChange={(e) => setCookieText(e.target.value)}
            />
            <div className="flex justify-end gap-2">
              <Button variant="ghost" onClick={() => setCookieMode(false)}>
                返回表单
              </Button>
              <Button disabled={busy} onClick={() => void saveCookie()}>
                保存 Cookie
              </Button>
            </div>
          </div>
        ) : (
          <div className="space-y-3 px-4 pb-4 md:px-5 md:pb-5">
            {fields.map((f) => (
              <label key={f.name} className="block space-y-1">
                <span className="text-sm text-muted-foreground">{f.label}</span>
                <Input
                  type={f.password ? "password" : "text"}
                  autoComplete="off"
                  value={values[f.name] ?? ""}
                  onChange={(e) =>
                    setValues((prev) => ({ ...prev, [f.name]: e.target.value }))
                  }
                />
              </label>
            ))}

            {captcha?.captchaUrl ? (
              <div className="space-y-2">
                <img
                  src={captcha.captchaUrl}
                  alt="验证码"
                  className="border-input h-12 rounded border bg-white"
                />
                <Input
                  placeholder="输入图中验证码"
                  value={captchaText}
                  onChange={(e) => setCaptchaText(e.target.value)}
                />
              </div>
            ) : null}

            {notice ? (
              <p className="text-destructive text-sm whitespace-pre-line">{notice}</p>
            ) : null}

            <div className="flex items-center justify-end gap-2">
              <Button variant="ghost" onClick={() => setCookieMode(true)}>
                <Cookie aria-hidden className="size-4" />
                手动 Cookie
              </Button>
              <Button
                disabled={busy}
                onClick={() => void submit(captcha ? captchaText : undefined)}
              >
                {busy ? <Spinner size="sm" /> : null}
                {captcha ? "提交验证码" : "登录"}
              </Button>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
