import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";
import { Navigate, useLocation, useNavigate, useSearchParams } from "react-router-dom";
import { fontFileUrl, UNAUTHORIZED_EVENT } from "@/api/client";
import { featuresApi } from "@/api/endpoints";
import type { CustomFont } from "@/api/types";
import { useAuthStore } from "@/stores/auth";
import { resolvePaperTheme, usePrefsStore } from "@/stores/prefs";

let bootstrapPromise: Promise<void> | null = null;
const loadedFontFaces = new Map<string, FontFace>();
const loadingFontIds = new Set<string>();

function ensureBootstrapped(): void {
  if (useAuthStore.getState().initialized || bootstrapPromise) return;
  bootstrapPromise = useAuthStore
    .getState()
    .bootstrap()
    .finally(() => {
      bootstrapPromise = null;
    });
}

function getSafeRedirect(raw: string | null): string {
  if (!raw) return "/";
  if (!raw.startsWith("/") || raw.startsWith("//")) return "/";
  if (raw.startsWith("/login")) return "/";
  return raw;
}

function buildLoginUrl(pathname: string, search: string, hash: string): string {
  const full = `${pathname}${search}${hash}`;
  if (!full || full === "/" || full.startsWith("/login")) {
    return "/login";
  }
  return `/login?redirect=${encodeURIComponent(full)}`;
}

/** 同步纸张主题至 documentElement（data-paper + .dark）并响应系统浅深色切换 */
function useThemeSync(): void {
  const theme = usePrefsStore((s) => s.theme);

  React.useEffect(() => {
    const apply = () => {
      const resolved = resolvePaperTheme(theme);
      const root = document.documentElement;
      root.dataset.paper = resolved;
      root.classList.toggle("dark", resolved === "dark");
    };

    apply();

    if (theme !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => apply();
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [theme]);
}

/** 启动鉴权初始化并加载全局主题同步 */
function useBootstrapAndTheme(): void {
  useThemeSync();

  React.useEffect(() => {
    ensureBootstrapped();
  }, []);
}

/** 拉取并注入用户上传的自定义字体，供全站与阅读器直接使用 */
function useCustomFonts(enabled: boolean): void {
  const { data: fonts } = useQuery<CustomFont[]>({
    queryKey: ["fonts"],
    queryFn: () => featuresApi.listFonts(),
    enabled,
    staleTime: 5 * 60_000,
    retry: 0,
  });

  React.useEffect(() => {
    if (!fonts || typeof FontFace === "undefined" || !("fonts" in document)) return;

    const activeIds = new Set(fonts.map((f) => f.id));
    for (const [id, face] of loadedFontFaces.entries()) {
      if (!activeIds.has(id)) {
        document.fonts.delete(face);
        loadedFontFaces.delete(id);
      }
    }

    for (const font of fonts) {
      if (!font.id || !font.family) continue;
      if (loadedFontFaces.has(font.id) || loadingFontIds.has(font.id)) continue;

      loadingFontIds.add(font.id);
      const url = fontFileUrl(font.id);
      try {
        const face = new FontFace(font.family, `url("${url}")`);
        void face
          .load()
          .then((loaded) => {
            document.fonts.add(loaded);
            loadedFontFaces.set(font.id, loaded);
          })
          .catch(() => {
            // 忽略损坏或不可达字体文件，避免影响其余字体
          })
          .finally(() => {
            loadingFontIds.delete(font.id);
          });
      } catch {
        loadingFontIds.delete(font.id);
      }
    }
  }, [fonts]);
}

function BootSplash() {
  return (
    <div className="flex min-h-dvh flex-col items-center justify-center gap-3 bg-bg text-ink">
      <div className="flex items-center gap-2 text-xs text-ink-muted">
        <span className="size-4 animate-spin rounded-full border-2 border-line-strong border-t-accent" />
        <span>正在加载…</span>
      </div>
    </div>
  );
}

export function AuthGate({ children }: { children: React.ReactNode }) {
  useBootstrapAndTheme();

  const initialized = useAuthStore((s) => s.initialized);
  const system = useAuthStore((s) => s.system);
  const user = useAuthStore((s) => s.user);

  const location = useLocation();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const locationRef = React.useRef(location);
  locationRef.current = location;

  React.useEffect(() => {
    const onUnauthorized = () => {
      useAuthStore.getState().clearSession();
      queryClient.clear();
      const loc = locationRef.current;
      if (loc.pathname !== "/login") {
        navigate(buildLoginUrl(loc.pathname, loc.search, loc.hash), { replace: true });
      }
    };
    window.addEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
    return () => window.removeEventListener(UNAUTHORIZED_EVENT, onUnauthorized);
  }, [navigate, queryClient]);

  const isMultiUser = system?.secure === true;
  const isAuthorized = initialized && (!isMultiUser || Boolean(user));

  useCustomFonts(isAuthorized);

  if (!initialized) {
    return <BootSplash />;
  }

  if (isMultiUser && !user) {
    return <Navigate to={buildLoginUrl(location.pathname, location.search, location.hash)} replace />;
  }

  return <>{children}</>;
}

export function GuestGate({ children }: { children: React.ReactNode }) {
  useBootstrapAndTheme();

  const initialized = useAuthStore((s) => s.initialized);
  const system = useAuthStore((s) => s.system);
  const user = useAuthStore((s) => s.user);
  const [searchParams] = useSearchParams();

  if (!initialized) {
    return <BootSplash />;
  }

  if (system?.secure === false) {
    return <Navigate to="/" replace />;
  }

  if (user) {
    return <Navigate to={getSafeRedirect(searchParams.get("redirect"))} replace />;
  }

  return <>{children}</>;
}
