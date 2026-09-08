import { Navigate, Route, Routes } from "react-router-dom";
import AppShell from "@/components/layout/AppShell";
import { GuestOnly, RequireAuth } from "@/components/layout/RequireAuth";
import { TooltipProvider, ToastViewport } from "@/components/ui";
import { useAuth } from "@/hooks/useAuth";
import { useTheme } from "@/hooks/useTheme";
import { lazyPage } from "@/lib/lazy";
import { useCustomFonts } from "@/hooks/useCustomFonts";

// 路由级分包: 每个页面单独一个 chunk, 首次访问才拉取;
// Suspense 占位与 chunk 失效(部署更新/断网)兜底见 @/lib/lazy.
const LoginPage = lazyPage(() => import("@/pages/LoginPage"));
const ShelfPage = lazyPage(() => import("@/pages/ShelfPage"));
const SearchPage = lazyPage(() => import("@/pages/SearchPage"));
const LibraryPage = lazyPage(() => import("@/pages/LibraryPage"));
const SourcesPage = lazyPage(() => import("@/pages/SourcesPage"));
const SourceWorkbenchPage = lazyPage(() => import("@/pages/SourceWorkbenchPage"));
const RssPage = lazyPage(() => import("@/pages/RssPage"));
const PurifyPage = lazyPage(() => import("@/pages/PurifyPage"));
const SettingsPage = lazyPage(() => import("@/pages/SettingsPage"));
const MePage = lazyPage(() => import("@/pages/MePage"));
const ReaderPage = lazyPage(() => import("@/pages/ReaderPage"));

export default function App() {
  useCustomFonts();
  // 自定义字体 @font-face 全局注入(阅读器/设置预览共用)
  // 主题应用到 <html>(dark class / data-theme), 全局一次
  useTheme();
  // 监听 reader:unauthorized → 清登录态并跳 /login?redirect=当前路径
  useAuth();

  return (
    <TooltipProvider>
      <Routes>
        <Route
          path="/login"
          element={
            <GuestOnly>
              <LoginPage />
            </GuestOnly>
          }
        />
        {/* 受保护组: AppShell 布局路由(顶栏/侧边栏, 移动端 slide-over 导航) */}
        <Route
          element={
            <RequireAuth>
              <AppShell />
            </RequireAuth>
          }
        >
          <Route path="/" element={<ShelfPage />} />
          <Route path="/search" element={<SearchPage />} />
          <Route path="/library" element={<LibraryPage />} />
          <Route path="/sources" element={<SourcesPage />} />
          <Route path="/workbench" element={<SourceWorkbenchPage />} />
          <Route path="/rss" element={<RssPage />} />
          <Route path="/purify" element={<PurifyPage />} />
          <Route path="/settings" element={<SettingsPage />} />
          <Route path="/me" element={<MePage />} />
        </Route>
        {/* 阅读器沉浸式全屏, 不进 AppShell, 单独受保护 */}
        <Route
          path="/reader"
          element={
            <RequireAuth>
              <ReaderPage />
            </RequireAuth>
          }
        />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
      {/* toast 视口挂 App 根: /login 与 /reader(壳外)也能弹, 且全局仅一份 */}
      <ToastViewport />
    </TooltipProvider>
  );
}
