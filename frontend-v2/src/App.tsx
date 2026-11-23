import { Suspense, lazy } from "react";
import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "@/shell/AppShell";
import { AuthGate, GuestGate } from "@/shell/AuthGate";
import { Toaster } from "@/ui/Toaster";

const LoginPage = lazy(() => import("@/features/auth/LoginPage"));
const LibraryPage = lazy(() => import("@/features/library/LibraryPage"));
const DiscoverPage = lazy(() => import("@/features/discover/DiscoverPage"));
const ExplorePage = lazy(() => import("@/features/discover/ExplorePage"));
const ReaderPage = lazy(() => import("@/features/reader/ReaderPage"));
const StudioLayout = lazy(() => import("@/features/studio/StudioLayout"));
const SourcesWorkspace = lazy(() => import("@/features/studio/SourcesWorkspace"));
const WorkbenchPage = lazy(() => import("@/features/studio/WorkbenchPage"));
const PurifyWorkspace = lazy(() => import("@/features/studio/PurifyWorkspace"));
const SettingsPage = lazy(() => import("@/features/settings/SettingsPage"));
const MePage = lazy(() => import("@/features/me/MePage"));

function PageLoader() {
  return (
    <div className="flex min-h-[45vh] items-center justify-center">
      <div className="h-5 w-5 animate-spin rounded-full border-2 border-line-strong border-t-accent" />
    </div>
  );
}

export default function App() {
  return (
    <>
      <Suspense fallback={<PageLoader />}>
        <Routes>
          <Route
            path="/login"
            element={
              <GuestGate>
                <LoginPage />
              </GuestGate>
            }
          />
          <Route
            element={
              <AuthGate>
                <AppShell />
              </AuthGate>
            }
          >
            <Route path="/" element={<LibraryPage />} />
            <Route path="/discover" element={<DiscoverPage />} />
            <Route path="/explore" element={<ExplorePage />} />
            <Route path="/me" element={<MePage />} />
            <Route path="/studio" element={<StudioLayout />}>
              <Route index element={<Navigate to="/studio/sources" replace />} />
              <Route path="sources" element={<SourcesWorkspace />} />
              <Route path="workbench" element={<WorkbenchPage />} />
              <Route path="purify" element={<PurifyWorkspace />} />
            </Route>
            <Route path="/settings" element={<SettingsPage />} />
            {/* 原版路由无缝兼容 */}
            <Route path="/search" element={<Navigate to="/discover" replace />} />
            <Route path="/library" element={<Navigate to="/explore" replace />} />
            <Route path="/sources" element={<Navigate to="/studio/sources" replace />} />
            <Route path="/workbench" element={<Navigate to="/studio/workbench" replace />} />
            <Route path="/purify" element={<Navigate to="/studio/purify" replace />} />
            <Route path="/users" element={<Navigate to="/settings?tab=account" replace />} />
          </Route>
          <Route
            path="/reader"
            element={
              <AuthGate>
                <ReaderPage />
              </AuthGate>
            }
          />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </Suspense>
      <Toaster />
    </>
  );
}
