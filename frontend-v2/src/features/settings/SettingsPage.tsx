import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Archive,
  ArrowUp,
  BookOpen,
  Check,
  ChevronRight,
  Clock,
  DatabaseZap,
  Download,
  File,
  FileArchive,
  Folder,
  HardDrive,
  History,
  LogOut,
  Pencil,
  Plus,
  RefreshCw,
  RotateCcw,
  Server,
  ShieldCheck,
  Trash2,
  Type,
  Upload,
  UserCog,
  Volume2,
} from "lucide-react";
import * as React from "react";
import { useNavigate, useSearchParams } from "react-router-dom";
import { fileDownloadUrl, fontFileUrl } from "@/api/client";
import { featuresApi } from "@/api/endpoints";
import type { CustomFont, HttpTts, RestoreReport } from "@/api/types";
import { cn } from "@/lib/cn";
import { formatBytes, formatRelativeTime, formatUptime } from "@/lib/format";
import { useAuthStore } from "@/stores/auth";
import {
  resolvePaperTheme,
  usePrefsStore,
  type PaperTheme,
  type ReaderFont,
  type TtsEngine,
} from "@/stores/prefs";
import { notify } from "@/stores/toast";
import { Button, IconButton } from "@/ui/Button";
import { Badge, EmptyView, Segmented, Slider, Spinner, Switch } from "@/ui/Controls";
import { Input, Textarea } from "@/ui/Input";
import { motion, PageEnter, SPRING_SNAPPY, useReducedMotion } from "@/ui/motion";
import { Sheet } from "@/ui/Sheet";
import { ConfirmAction, ConfirmDialog, UsersSection } from "./UsersSection";

type SettingsTab = "reading" | "voice" | "storage" | "account";

const SETTINGS_TABS: readonly {
  id: SettingsTab;
  label: string;
  desc: string;
  icon: React.ComponentType<{ className?: string }>;
}[] = [
  { id: "reading", label: "阅读与排版", desc: "纸色、字体与搜索超时", icon: BookOpen },
  { id: "voice", label: "听书语音", desc: "朗读引擎与 HttpTTS", icon: Volume2 },
  { id: "storage", label: "缓存与备份", desc: "服务端缓存与 WebDAV", icon: HardDrive },
  { id: "account", label: "账号与管理", desc: "运行状态与用户权限", icon: UserCog },
];

const PAPER_SWATCHES: readonly {
  value: PaperTheme;
  label: string;
  bg: string;
  dot: string;
}[] = [
  { value: "system", label: "跟随系统", bg: "linear-gradient(135deg,#fcfaf6 50%,#171513 50%)", dot: "#8c3b28" },
  { value: "paper", label: "日间", bg: "#fcfaf6", dot: "#26221e" },
  { value: "parchment", label: "羊皮纸", bg: "#f6eeda", dot: "#433422" },
  { value: "celadon", label: "护眼绿", bg: "#edf4ee", dot: "#223529" },
  { value: "dark", label: "夜间", bg: "#171513", dot: "#e8e4dc" },
];

const EDGE_VOICE_PRESETS = [
  { value: "zh-CN-XiaoxiaoNeural", label: "晓晓 · 温润女声" },
  { value: "zh-CN-YunxiNeural", label: "云希 · 清朗男声" },
  { value: "zh-CN-YunjianNeural", label: "云健 · 沉稳播讲" },
  { value: "zh-CN-XiaoyiNeural", label: "晓伊 · 灵动女声" },
  { value: "zh-CN-YunyangNeural", label: "云扬 · 新闻播音" },
];

interface HttpTtsDraft {
  isNew: boolean;
  name: string;
  url: string;
  type: number;
  contentType: string;
  concurrentRate: string;
  header: string;
  loginUrl: string;
  loginUi: string;
  jsLib: string;
  loginCheckJs: string;
  enabledCookieJar: boolean;
}

const EMPTY_TTS_DRAFT: HttpTtsDraft = {
  isNew: true,
  name: "",
  url: "",
  type: 0,
  contentType: "audio/mpeg",
  concurrentRate: "",
  header: "",
  loginUrl: "",
  loginUi: "",
  jsLib: "",
  loginCheckJs: "",
  enabledCookieJar: false,
};

function ttsDraftDirty(a: HttpTtsDraft | null, b: HttpTtsDraft | null): boolean {
  if (!a || !b) return false;
  return (Object.keys(a) as (keyof HttpTtsDraft)[]).some((k) => a[k] !== b[k]);
}

function errText(err: unknown): string {
  return err instanceof Error ? err.message : "操作失败";
}

function summarizeRestore(report: RestoreReport): string {
  const r = report.restored;
  const s = report.skipped;
  const parts: string[] = [];
  if (r.books) parts.push(`书籍 ${r.books}`);
  if (r.sources) parts.push(`书源 ${r.sources}`);
  if (r.bookmarks) parts.push(`书签 ${r.bookmarks}`);
  if (r.groups) parts.push(`分组 ${r.groups}`);
  if (r.rules) parts.push(`净化 ${r.rules}`);
  if (r.httpTts) parts.push(`音源 ${r.httpTts}`);
  const skippedTotal =
    (s.books || 0) +
    (s.sources || 0) +
    (s.bookmarks || 0) +
    (s.groups || 0) +
    (s.rules || 0) +
    (s.httpTts || 0);
  const restoredText = parts.length > 0 ? parts.join(" · ") : "无新增变更";
  return skippedTotal > 0
    ? `恢复完成：${restoredText}（跳过 ${skippedTotal} 项）`
    : `恢复完成：${restoredText}`;
}

function resolveTab(raw: string | null): SettingsTab {
  if (raw === "reading" || raw === "voice" || raw === "storage" || raw === "account") {
    return raw;
  }
  return "reading";
}

function normalizeRemotePath(raw: string): string {
  const trimmed = raw.trim().replace(/\/+/g, "/");
  if (!trimmed || trimmed === "/") return "/";
  const withLead = trimmed.startsWith("/") ? trimmed : `/${trimmed}`;
  return withLead.endsWith("/") ? withLead.slice(0, -1) : withLead;
}

function parentRemotePath(current: string): string {
  const norm = normalizeRemotePath(current);
  if (norm === "/") return "/";
  const idx = norm.lastIndexOf("/");
  return idx <= 0 ? "/" : norm.slice(0, idx);
}

/* ==========================================================================
   Layout primitives: section / stack / card / row
   ========================================================================== */

function SettingsSection({
  id,
  title,
  nav,
  desc,
  badge,
  actions,
  children,
  className,
}: {
  id: string;
  title: React.ReactNode;
  nav?: string;
  desc?: React.ReactNode;
  badge?: React.ReactNode;
  actions?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section
      id={`settings-${id}`}
      data-settings-section=""
      data-label={nav ?? (typeof title === "string" ? title : id)}
      className={cn("flex scroll-mt-32 flex-col gap-4 md:scroll-mt-6", className)}
    >
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h2 className="font-serif text-[17px] font-semibold tracking-tight text-ink">{title}</h2>
            {badge}
          </div>
          {desc ? <p className="mt-1 text-xs leading-relaxed text-ink-muted">{desc}</p> : null}
        </div>
        {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
      </header>
      {children}
    </section>
  );
}

function SectionStack({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-8 [&>section+section]:border-t [&>section+section]:border-line [&>section+section]:pt-8">
      {children}
    </div>
  );
}

function SettingsCard({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "divide-y divide-line/70 border-y border-line/70 bg-transparent md:divide-line md:overflow-hidden md:rounded-2xl md:border md:border-line md:bg-elevated",
        className,
      )}
    >
      {children}
    </div>
  );
}

function CardGroupLabel({ children }: { children: React.ReactNode }) {
  return (
    <p className="px-0.5 py-2 text-[11px] font-semibold tracking-wider text-ink-muted uppercase md:bg-subtle/35 md:px-4">
      {children}
    </p>
  );
}

function SettingRow({
  title,
  desc,
  control,
}: {
  title: React.ReactNode;
  desc?: React.ReactNode;
  control: React.ReactNode;
}) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-3 px-0.5 py-3.5 md:px-4">
      <div className="min-w-0">
        <p className="text-[13px] font-medium text-ink">{title}</p>
        {desc ? <p className="mt-0.5 text-[11px] text-ink-muted">{desc}</p> : null}
      </div>
      {control}
    </div>
  );
}

/* ==========================================================================
   Scroll spy over [data-settings-section]
   ========================================================================== */

interface SectionAnchor {
  id: string;
  label: string;
}

function findScrollParent(el: HTMLElement | null): HTMLElement | null {
  let node = el?.parentElement ?? null;
  while (node && node !== document.body) {
    const oy = getComputedStyle(node).overflowY;
    if ((oy === "auto" || oy === "scroll") && node.scrollHeight > node.clientHeight) return node;
    node = node.parentElement;
  }
  return null;
}

function useSectionSpy(contentRef: React.RefObject<HTMLDivElement | null>, tab: SettingsTab) {
  const [anchors, setAnchors] = React.useState<SectionAnchor[]>([]);
  const [activeId, setActiveId] = React.useState<string>("");
  const lockUntil = React.useRef(0);
  const activeIdRef = React.useRef(activeId);
  activeIdRef.current = activeId;

  React.useLayoutEffect(() => {
    const root = contentRef.current;
    if (!root) return;
    const collect = () => {
      const next = Array.from(root.querySelectorAll<HTMLElement>("[data-settings-section]")).map((el) => ({
        id: el.id,
        label: el.dataset.label ?? el.id,
      }));
      setAnchors((prev) =>
        prev.length === next.length && prev.every((p, i) => p.id === next[i]?.id && p.label === next[i]?.label)
          ? prev
          : next,
      );
    };
    collect();
    const mo = new MutationObserver(collect);
    mo.observe(root, { childList: true, subtree: true });
    return () => mo.disconnect();
  }, [contentRef, tab]);

  React.useEffect(() => {
    setActiveId("");
  }, [tab]);

  React.useEffect(() => {
    const root = contentRef.current;
    if (!root || anchors.length === 0) return;
    const scrollRoot = findScrollParent(root);
    const topOffset = window.innerWidth < 768 ? 120 : 32;
    const visible = new Map<string, boolean>();

    const pick = () => {
      if (Date.now() < lockUntil.current) return;
      const sc = scrollRoot ?? document.documentElement;
      if (sc.scrollHeight - sc.scrollTop - sc.clientHeight < 4 && sc.scrollTop > 0) {
        const last = anchors[anchors.length - 1];
        if (last) setActiveId(last.id);
        return;
      }
      const first = anchors.find((a) => visible.get(a.id));
      if (first) setActiveId(first.id);
    };

    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) visible.set(e.target.id, e.isIntersecting);
        pick();
      },
      { root: scrollRoot, rootMargin: `-${topOffset}px 0px -55% 0px`, threshold: 0 },
    );
    for (const a of anchors) {
      const el = document.getElementById(a.id);
      if (el) io.observe(el);
    }
    const target: HTMLElement | Window = scrollRoot ?? window;
    target.addEventListener("scroll", pick, { passive: true });
    if (!activeIdRef.current && anchors[0]) setActiveId(anchors[0].id);
    return () => {
      io.disconnect();
      target.removeEventListener("scroll", pick);
    };
  }, [anchors, contentRef]);

  const jumpTo = React.useCallback(
    (id: string, smooth = true) => {
      const el = document.getElementById(id);
      if (!el) return;
      lockUntil.current = Date.now() + 750;
      setActiveId(id);
      const scrollRoot = findScrollParent(contentRef.current);
      const offset = window.innerWidth < 768 ? 104 : 24;
      const behavior: ScrollBehavior = smooth ? "smooth" : "auto";
      if (scrollRoot) {
        const rootRect = scrollRoot.getBoundingClientRect();
        const elRect = el.getBoundingClientRect();
        const top = Math.max(0, scrollRoot.scrollTop + (elRect.top - rootRect.top) - offset);
        scrollRoot.scrollTo({ top, behavior });
      } else {
        const top = Math.max(0, window.scrollY + el.getBoundingClientRect().top - offset);
        window.scrollTo({ top, behavior });
      }
    },
    [contentRef],
  );

  return { anchors, activeId: activeId || anchors[0]?.id || "", jumpTo };
}

/* ==========================================================================
   Page
   ========================================================================== */

export default function SettingsPage() {
  const [searchParams, setSearchParams] = useSearchParams();
  const activeTab = resolveTab(searchParams.get("tab"));
  const sectionParam = searchParams.get("section");
  const reduced = useReducedMotion();
  const contentRef = React.useRef<HTMLDivElement>(null);
  const tabStripRef = React.useRef<HTMLDivElement>(null);
  const sectionStripRef = React.useRef<HTMLDivElement>(null);

  const { anchors, activeId, jumpTo } = useSectionSpy(contentRef, activeTab);

  const selectTab = (tab: SettingsTab) => {
    if (tab === activeTab) {
      if (anchors[0]) jumpTo(anchors[0].id, !reduced);
      return;
    }
    setSearchParams({ tab }, { replace: true });
    const sc = findScrollParent(contentRef.current);
    (sc ?? window).scrollTo({ top: 0 });
  };

  // Deep-link: ?tab=storage&section=webdav
  const deepLinked = React.useRef<string | null>(null);
  React.useEffect(() => {
    if (!sectionParam || anchors.length === 0) return;
    const key = `${activeTab}:${sectionParam}`;
    if (deepLinked.current === key) return;
    const id = `settings-${sectionParam}`;
    if (anchors.some((a) => a.id === id)) {
      deepLinked.current = key;
      window.setTimeout(() => jumpTo(id, false), 60);
    }
  }, [sectionParam, anchors, activeTab, jumpTo]);

  // Keep active mobile chip visible horizontally without touching vertical scroll (avoids cancelling jumpTo)
  React.useEffect(() => {
    const reveal = (strip: HTMLDivElement | null, selector: string) => {
      const chip = strip?.querySelector<HTMLElement>(selector);
      if (!strip || !chip) return;
      const pad = 12;
      const chipLeft = chip.offsetLeft;
      const chipRight = chipLeft + chip.offsetWidth;
      const viewLeft = strip.scrollLeft;
      const viewRight = viewLeft + strip.clientWidth;
      if (chipLeft < viewLeft + pad) {
        strip.scrollTo({
          left: Math.max(0, chipLeft - pad),
          behavior: reduced ? "auto" : "smooth",
        });
      } else if (chipRight > viewRight - pad) {
        strip.scrollTo({
          left: chipRight - strip.clientWidth + pad,
          behavior: reduced ? "auto" : "smooth",
        });
      }
    };
    reveal(tabStripRef.current, "[data-active='true']");
    reveal(sectionStripRef.current, "[data-active='true']");
  }, [activeTab, activeId, reduced]);

  return (
    <PageEnter className="mx-auto w-full max-w-[1120px] px-4 pt-1 pb-14 sm:px-6 md:pt-5 lg:px-8">
      <header className="mb-7 hidden border-b border-line pb-4 md:block">
        <h1 className="font-serif text-2xl font-semibold tracking-tight text-ink sm:text-[26px]">
          偏好与系统设置
        </h1>
        <p className="mt-1 text-xs text-ink-muted">
          定制阅读纸张与字体、配置听书音源、管理服务端缓存与备份
        </p>
      </header>

      {/* Mobile: sticky underline tabs + quiet section chips (z-20 above content controls, strictly below AppShell header z-30 & Popover portal z-80) */}
      <div className="sticky top-0 z-20 -mx-4 mb-5 border-b border-line bg-bg/95 px-4 pt-1 pb-2 backdrop-blur-md sm:-mx-6 sm:px-6 md:hidden">
        <div
          ref={tabStripRef}
          className="scroll-fade-x flex gap-5 overflow-x-auto border-b border-line/60 pr-3 no-scrollbar"
          role="tablist"
          aria-label="设置分类"
        >
          {SETTINGS_TABS.map((t) => {
            const Icon = t.icon;
            const active = t.id === activeTab;
            return (
              <button
                key={t.id}
                type="button"
                role="tab"
                aria-selected={active}
                data-active={active}
                onClick={() => selectTab(t.id)}
                className={cn(
                  "inline-flex shrink-0 cursor-pointer items-center gap-1.5 border-b-2 py-2 text-[13px] whitespace-nowrap transition-colors",
                  active
                    ? "border-accent font-semibold text-accent"
                    : "border-transparent text-ink-secondary hover:text-ink",
                )}
              >
                <Icon className="size-3.5" />
                {t.label}
              </button>
            );
          })}
        </div>
        {anchors.length > 1 ? (
          <div
            ref={sectionStripRef}
            className="mt-2 flex gap-1 overflow-x-auto no-scrollbar"
            aria-label="本页分节"
          >
            {anchors.map((a) => {
              const active = a.id === activeId;
              return (
                <button
                  key={a.id}
                  type="button"
                  data-active={active}
                  onClick={() => jumpTo(a.id, !reduced)}
                  className={cn(
                    "relative shrink-0 cursor-pointer rounded-full px-2.5 py-1 text-[11px] whitespace-nowrap transition-colors",
                    active ? "font-medium text-ink" : "text-ink-muted hover:text-ink-secondary",
                  )}
                >
                  {active ? (
                    <motion.span
                      layoutId="v2-settings-section-chip"
                      transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                      className="absolute inset-0 -z-10 rounded-full bg-subtle ring-1 ring-line"
                    />
                  ) : null}
                  {a.label}
                </button>
              );
            })}
          </div>
        ) : null}
      </div>

      <div className="flex flex-col gap-8 md:flex-row md:items-start lg:gap-10">
        {/* Desktop: sticky rail with tabs + in-page anchors */}
        <aside className="hidden w-52 shrink-0 md:sticky md:top-6 md:block">
          <nav aria-label="设置分类" className="flex flex-col gap-1">
            {SETTINGS_TABS.map((item) => {
              const Icon = item.icon;
              const isActive = activeTab === item.id;
              return (
                <div key={item.id}>
                  <button
                    type="button"
                    onClick={() => selectTab(item.id)}
                    aria-current={isActive ? "page" : undefined}
                    className={cn(
                      "relative flex w-full cursor-pointer items-start gap-3 rounded-2xl px-3.5 py-2.5 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60",
                      isActive ? "text-ink" : "text-ink-secondary hover:bg-subtle/60 hover:text-ink",
                    )}
                  >
                    {isActive ? (
                      <motion.span
                        layoutId="v2-settings-rail"
                        transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                        className="pointer-events-none absolute inset-0 -z-10 rounded-2xl border border-line bg-elevated shadow-[var(--shadow-sm)]"
                      />
                    ) : null}
                    <Icon
                      className={cn("mt-0.5 size-4 shrink-0", isActive ? "text-accent" : "text-ink-muted")}
                    />
                    <div className="min-w-0">
                      <p className="text-[13px] font-medium">{item.label}</p>
                      <p className="mt-0.5 truncate text-[11px] text-ink-muted">{item.desc}</p>
                    </div>
                  </button>

                  {isActive && anchors.length > 1 ? (
                    <motion.ul
                      initial={reduced ? false : { opacity: 0, height: 0 }}
                      animate={{ opacity: 1, height: "auto" }}
                      transition={SPRING_SNAPPY}
                      className="relative mt-1 mb-2 ml-[22px] flex flex-col overflow-hidden border-l border-line py-0.5"
                      aria-label="本页分节"
                    >
                      {anchors.map((a) => {
                        const active = a.id === activeId;
                        return (
                          <li key={a.id} className="relative">
                            {active ? (
                              <motion.span
                                layoutId="v2-settings-anchor"
                                transition={reduced ? { duration: 0 } : SPRING_SNAPPY}
                                className="absolute inset-y-1 -left-px w-0.5 rounded-full bg-accent"
                              />
                            ) : null}
                            <a
                              href={`#${a.id}`}
                              aria-current={active ? "location" : undefined}
                              onClick={(e) => {
                                e.preventDefault();
                                jumpTo(a.id, !reduced);
                              }}
                              className={cn(
                                "block truncate py-1.5 pr-2 pl-3.5 text-xs transition-colors",
                                active ? "font-medium text-accent" : "text-ink-muted hover:text-ink",
                              )}
                            >
                              {a.label}
                            </a>
                          </li>
                        );
                      })}
                    </motion.ul>
                  ) : null}
                </div>
              );
            })}
          </nav>
        </aside>

        <div ref={contentRef} role="region" aria-label="设置内容" className="min-w-0 max-w-[740px] flex-1">
          {activeTab === "reading" ? <ReadingSection /> : null}
          {activeTab === "voice" ? <VoiceSection /> : null}
          {activeTab === "storage" ? <StorageSection /> : null}
          {activeTab === "account" ? <AccountSection /> : null}
        </div>
      </div>
    </PageEnter>
  );
}

/* ==========================================================================
   1. Reading & Typography + Custom Fonts
   ========================================================================== */

function ReadingSection() {
  const qc = useQueryClient();
  const prefs = usePrefsStore();
  const fileInputRef = React.useRef<HTMLInputElement>(null);
  const [draggingFont, setDraggingFont] = React.useState(false);

  const fontsQuery = useQuery({
    queryKey: ["settings", "fonts"],
    queryFn: () => featuresApi.listFonts(),
  });
  const fonts = React.useMemo(() => fontsQuery.data ?? [], [fontsQuery.data]);

  // Dynamically load active custom font into document.fonts for live paper preview
  React.useEffect(() => {
    if (prefs.fontFamily !== "custom" || !prefs.customFontFamily) return;
    const target = fonts.find((f) => f.family === prefs.customFontFamily);
    if (!target || typeof FontFace === "undefined") return;
    let active = true;
    const face = new FontFace(target.family, `url(${fontFileUrl(target.id)})`);
    void face
      .load()
      .then((loaded) => {
        if (active) document.fonts.add(loaded);
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [prefs.fontFamily, prefs.customFontFamily, fonts]);

  const uploadFontMutation = useMutation({
    mutationFn: (file: File) => featuresApi.uploadFont(file),
    onSuccess: (font) => {
      notify.success(`字体「${font.name}」上传成功`);
      void qc.invalidateQueries({ queryKey: ["settings", "fonts"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const deleteFontMutation = useMutation({
    mutationFn: (font: CustomFont) => featuresApi.deleteFont(font.id),
    onSuccess: (_res, font) => {
      notify.success(`已删除字体「${font.name}」`);
      if (prefs.customFontFamily === font.family) {
        prefs.setFontFamily("serif");
        prefs.setCustomFontFamily("");
      }
      void qc.invalidateQueries({ queryKey: ["settings", "fonts"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const handleFontFiles = (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    const file = fileList[0];
    if (!file) return;
    if (!/\.(ttf|otf|woff2?)$/i.test(file.name)) {
      notify.error("仅支持 .ttf / .otf / .woff / .woff2 字体文件");
      return;
    }
    uploadFontMutation.mutate(file);
  };

  const applyCustomFont = (font: CustomFont) => {
    prefs.setCustomFontFamily(font.family);
    prefs.setFontFamily("custom");
    notify.info(`已切换默认字体为「${font.name}」`);
  };

  const previewPaper = resolvePaperTheme(prefs.theme);
  const previewFontCss =
    prefs.fontFamily === "custom" && prefs.customFontFamily
      ? `"${prefs.customFontFamily}", var(--font-serif)`
      : prefs.fontFamily === "kai"
        ? "var(--font-kai)"
        : prefs.fontFamily === "sans"
          ? "var(--font-sans)"
          : "var(--font-serif)";

  const fontOptions: { value: ReaderFont; label: string }[] = [
    { value: "serif", label: "宋体" },
    { value: "kai", label: "楷体" },
    { value: "sans", label: "黑体" },
    ...(fonts.length > 0 || prefs.customFontFamily
      ? [{ value: "custom" as ReaderFont, label: "自定义" }]
      : []),
  ];

  return (
    <SectionStack>
      {/* Live Paper Preview */}
      <SettingsSection
        id="preview"
        title="纸张与排版预览"
        nav="排版预览"
        desc="所有调整将实时呈现于下方书页样张"
        actions={
          <ConfirmAction
            tone="warn"
            icon={<RotateCcw className="size-[18px]" />}
            title="恢复默认排版参数？"
            description="将排版与翻页参数重置为系统推荐值。"
            consequences={[
              "字号、行距、段落间距、版心宽度、首行缩进与翻页偏好将恢复出厂设置",
              "当前纸色主题与已上传的自定义字体不受影响",
            ]}
            confirmLabel="确认恢复"
            onConfirm={() => {
              prefs.resetTypography();
              notify.info("已恢复默认排版参数");
            }}
            trigger={(open) => (
              <Button variant="ghost" size="xs" onClick={open}>
                <RotateCcw className="size-3" />
                恢复默认排版
              </Button>
            )}
          />
        }
      >
        <div
          data-paper={previewPaper}
          className="overflow-hidden rounded-2xl border border-line bg-[var(--bg)] px-6 py-5 text-[var(--ink)] shadow-[var(--shadow-sm)] transition-colors duration-300 sm:px-8 sm:py-6"
        >
          <div className="mb-3 flex flex-wrap items-center justify-between gap-2 text-[11px] text-[var(--ink-muted)] tabular-nums">
            <span>卷一 · 寒江夜雨</span>
            <span>
              {prefs.fontSize}px · 行距 {prefs.lineHeight.toFixed(2)} ·{" "}
              {prefs.readLayout === "paged"
                ? prefs.pageTurnMode === "curl"
                  ? "仿真翻页"
                  : "无动画翻页"
                : "连续滚动"}
            </span>
          </div>
          <div
            style={{
              fontFamily: previewFontCss,
              fontSize: `${prefs.fontSize}px`,
              lineHeight: prefs.lineHeight,
            }}
          >
            <p
              style={{
                textIndent: prefs.indentParagraph ? "2em" : undefined,
                marginBottom: `${prefs.paragraphGap}em`,
              }}
            >
              檐角细雨敲打青瓦，案头一盏松烟孤灯微微摇曳。翻开泛黄的线装旧卷，墨痕如新，字里行间皆是山川风月。
            </p>
            <p
              style={{
                textIndent: prefs.indentParagraph ? "2em" : undefined,
              }}
            >
              阅读本该如是——卸下浮躁与喧嚣，让文字在恰到好处的留白中徐徐呼吸。
            </p>
          </div>
        </div>
      </SettingsSection>

      {/* Paper & Typography Controls */}
      <SettingsSection
        id="typography"
        title="默认纸色与排版"
        nav="纸色与排版"
        desc="定制默认阅读纸张、字体族、版心留白与翻页动效"
      >
        {/* Paper Swatches */}
        <div className="grid grid-cols-3 gap-2.5 sm:grid-cols-5">
          {PAPER_SWATCHES.map((swatch) => {
            const active = prefs.theme === swatch.value;
            return (
              <button
                key={swatch.value}
                type="button"
                aria-pressed={active}
                onClick={() => prefs.setTheme(swatch.value)}
                className={cn(
                  "flex cursor-pointer flex-col items-center gap-2 rounded-2xl border px-3 py-3 transition-all outline-none focus-visible:ring-2 focus-visible:ring-accent/60",
                  active
                    ? "border-accent bg-accent-soft/45 ring-1 ring-accent/30"
                    : "border-line bg-elevated hover:border-line-strong",
                )}
              >
                <span
                  style={{ background: swatch.bg }}
                  className="relative flex size-8 items-center justify-center rounded-full border border-black/15 shadow-xs"
                >
                  {active ? (
                    <Check className="size-3.5 text-accent drop-shadow-xs" />
                  ) : (
                    <span
                      style={{ backgroundColor: swatch.dot }}
                      className="size-1.5 rounded-full opacity-55"
                    />
                  )}
                </span>
                <span className="text-xs font-medium text-ink">{swatch.label}</span>
              </button>
            );
          })}
        </div>

        {/* Font & Typography Card */}
        <SettingsCard>
          <CardGroupLabel>字体与版式</CardGroupLabel>
          <SettingRow
            title="阅读字体"
            desc="选择正文字体族，或选用下方上传的自定字体"
            control={
              <Segmented
                layoutId="v2-pref-font"
                size="sm"
                value={prefs.fontFamily}
                onChange={(v) => {
                  if (v === "custom" && !prefs.customFontFamily && fonts[0]) {
                    prefs.setCustomFontFamily(fonts[0].family);
                  }
                  prefs.setFontFamily(v);
                }}
                options={fontOptions}
              />
            }
          />

          <div className="grid grid-cols-1 gap-4 px-4 py-4 sm:grid-cols-2">
            <div>
              <div className="mb-1.5 flex items-center justify-between text-xs">
                <span className="font-medium text-ink-secondary">默认字号</span>
                <span className="font-mono text-ink tabular-nums">{prefs.fontSize} px</span>
              </div>
              <Slider
                ariaLabel="默认字号"
                min={14}
                max={32}
                step={1}
                value={prefs.fontSize}
                onChange={prefs.setFontSize}
              />
            </div>

            <div>
              <div className="mb-1.5 flex items-center justify-between text-xs">
                <span className="font-medium text-ink-secondary">行距倍数</span>
                <span className="font-mono text-ink tabular-nums">
                  {prefs.lineHeight.toFixed(2)}
                </span>
              </div>
              <Slider
                ariaLabel="行距倍数"
                min={1.4}
                max={2.4}
                step={0.05}
                value={prefs.lineHeight}
                onChange={prefs.setLineHeight}
              />
            </div>

            <div>
              <div className="mb-1.5 flex items-center justify-between text-xs">
                <span className="font-medium text-ink-secondary">段落间距</span>
                <span className="font-mono text-ink tabular-nums">
                  {prefs.paragraphGap.toFixed(1)} em
                </span>
              </div>
              <Slider
                ariaLabel="段落间距"
                min={0.2}
                max={1.8}
                step={0.1}
                value={prefs.paragraphGap}
                onChange={prefs.setParagraphGap}
              />
            </div>

            <div>
              <div className="mb-1.5 flex items-center justify-between text-xs">
                <span className="font-medium text-ink-secondary">单栏版心宽度</span>
                <span className="font-mono text-ink tabular-nums">{prefs.contentWidth} em</span>
              </div>
              <Slider
                ariaLabel="版心宽度"
                min={32}
                max={62}
                step={1}
                value={prefs.contentWidth}
                onChange={prefs.setContentWidth}
              />
            </div>
          </div>

          <SettingRow
            title="段落首行缩进"
            desc="中文排印传统两字空距"
            control={
              <Switch
                checked={prefs.indentParagraph}
                ariaLabel="段落首行缩进"
                onChange={prefs.setIndentParagraph}
              />
            }
          />
        </SettingsCard>

        {/* Page Turn & Layout Card */}
        <SettingsCard>
          <CardGroupLabel>翻页与阅读交互</CardGroupLabel>
          <SettingRow
            title="默认翻页方式"
            desc="分页书本感或连续上下滚动"
            control={
              <Segmented
                layoutId="v2-pref-layout"
                size="sm"
                value={prefs.readLayout}
                onChange={prefs.setReadLayout}
                options={[
                  { value: "paged", label: "翻页模式" },
                  { value: "scroll", label: "滚动模式" },
                ]}
              />
            }
          />
          <SettingRow
            title="书卷翻页动效"
            desc="纸书仿真卷页光影，或墨水屏式极速无动画直切"
            control={
              <Segmented
                layoutId="v2-pref-pageturn"
                size="sm"
                value={prefs.pageTurnMode}
                onChange={prefs.setPageTurnMode}
                options={[
                  { value: "curl", label: "仿真翻页" },
                  { value: "none", label: "无动画" },
                ]}
              />
            }
          />
          <SettingRow
            title="宽屏双页对开"
            desc="桌面宽屏翻页时自动展开左右双栏"
            control={
              <Segmented
                layoutId="v2-pref-spread"
                size="sm"
                value={prefs.spreadMode}
                onChange={prefs.setSpreadMode}
                options={[
                  { value: "auto", label: "自动双页" },
                  { value: "single", label: "始终单页" },
                ]}
              />
            }
          />
        </SettingsCard>
      </SettingsSection>

      {/* Search Timeout */}
      <SettingsSection
        id="search"
        title="搜书网络偏好"
        nav="搜书超时"
        desc="控制多源并发搜书时单个书源的最大等待时长"
        badge={<Badge tone="accent">{prefs.searchTimeout} 秒</Badge>}
      >
        <SettingsCard>
          <div className="px-4 py-4">
            <div className="mb-1.5 flex items-center justify-between text-xs text-ink-secondary">
              <span>单源超时时间（3 – 60 秒）</span>
              <span className="font-mono text-ink tabular-nums">{prefs.searchTimeout} s</span>
            </div>
            <Slider
              ariaLabel="单源搜索超时时间"
              min={3}
              max={60}
              step={1}
              value={prefs.searchTimeout}
              onChange={prefs.setSearchTimeout}
            />
          </div>
        </SettingsCard>
      </SettingsSection>

      {/* Custom Fonts Management */}
      <SettingsSection
        id="fonts"
        title="自定义字体"
        nav="自定义字体"
        badge={fonts.length > 0 ? <Badge>{fonts.length}</Badge> : undefined}
        desc="上传专属中文字体至服务端，跨设备同步使用（支持 .ttf / .otf / .woff / .woff2，上限 40 MB）"
        actions={
          <>
            <Button
              variant="secondary"
              size="sm"
              loading={uploadFontMutation.isPending}
              onClick={() => fileInputRef.current?.click()}
            >
              <Upload className="size-3.5" />
              上传字体
            </Button>
            <input
              ref={fileInputRef}
              type="file"
              accept=".ttf,.otf,.woff,.woff2"
              className="hidden"
              onChange={(e) => {
                handleFontFiles(e.target.files);
                e.target.value = "";
              }}
            />
          </>
        }
      >
        {/* Drag & drop area */}
        <div
          onDragOver={(e) => {
            e.preventDefault();
            setDraggingFont(true);
          }}
          onDragLeave={() => setDraggingFont(false)}
          onDrop={(e) => {
            e.preventDefault();
            setDraggingFont(false);
            handleFontFiles(e.dataTransfer.files);
          }}
          onClick={() => fileInputRef.current?.click()}
          className={cn(
            "flex cursor-pointer flex-col items-center justify-center gap-1.5 rounded-2xl border border-dashed px-4 py-5 text-center transition-colors",
            draggingFont
              ? "border-accent bg-accent-soft/45"
              : "border-line-strong bg-subtle/35 hover:border-accent/50 hover:bg-subtle/65",
          )}
        >
          <Type className="size-5 text-ink-muted" />
          <p className="text-xs font-medium text-ink">拖拽字体文件至此处，或点击选择上传</p>
          <p className="text-[11px] text-ink-muted">推荐使用霞鹜文楷、思源宋体等开源字体</p>
        </div>

        {fontsQuery.isLoading ? (
          <div className="flex justify-center py-6">
            <Spinner label="加载字体列表…" />
          </div>
        ) : fonts.length === 0 ? (
          <div className="rounded-2xl border border-dashed border-line-strong bg-subtle/25">
            <EmptyView
              compact
              icon={<Type className="size-5" />}
              title="尚未上传自定义字体"
              description="上传后的字体可在阅读设置与上方样张中即刻启用"
            />
          </div>
        ) : (
          <SettingsCard>
            {fonts.map((font) => {
              const isSelected =
                prefs.fontFamily === "custom" && prefs.customFontFamily === font.family;
              return (
                <div
                  key={font.id}
                  className="flex flex-wrap items-center justify-between gap-3 px-4 py-3 transition-colors hover:bg-subtle/30"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <p className="truncate text-[13px] font-medium text-ink">{font.name}</p>
                      {isSelected ? <Badge tone="accent">当前使用</Badge> : null}
                    </div>
                    <p className="mt-0.5 truncate font-mono text-[11px] text-ink-muted tabular-nums">
                      {font.family} · {formatBytes(font.size)}
                      {font.updatedAt ? ` · ${formatRelativeTime(font.updatedAt)}` : ""}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1.5">
                    {!isSelected ? (
                      <Button variant="subtle" size="xs" onClick={() => applyCustomFont(font)}>
                        设为阅读字体
                      </Button>
                    ) : null}
                    <ConfirmAction
                      title={`删除字体「${font.name}」？`}
                      description={`字体文件（${formatBytes(font.size)}）将从服务端永久移除。`}
                      consequences={[
                        "所有已登录设备将无法继续加载该自定义字体",
                        ...(isSelected
                          ? ["当前阅读字体正在使用该字体，删除后将自动回退为默认「宋体」"]
                          : []),
                      ]}
                      confirmLabel="确认删除"
                      icon={<Trash2 className="size-[18px]" />}
                      busy={deleteFontMutation.isPending}
                      onConfirm={() => deleteFontMutation.mutateAsync(font)}
                      trigger={(open) => (
                        <IconButton
                          size="sm"
                          tone="danger"
                          aria-label={`删除字体 ${font.name}`}
                          onClick={open}
                        >
                          <Trash2 className="size-3.5" />
                        </IconButton>
                      )}
                    />
                  </div>
                </div>
              );
            })}
          </SettingsCard>
        )}
      </SettingsSection>
    </SectionStack>
  );
}

/* ==========================================================================
   2. Voice / TTS + HttpTTS Sources
   ========================================================================== */

function VoiceSection() {
  const qc = useQueryClient();
  const prefs = usePrefsStore();
  const [ttsDraft, setTtsDraft] = React.useState<HttpTtsDraft | null>(null);
  const [ttsDraftBase, setTtsDraftBase] = React.useState<HttpTtsDraft | null>(null);
  const [discardTtsAsk, setDiscardTtsAsk] = React.useState(false);
  const [browserVoices, setBrowserVoices] = React.useState<string[]>([]);

  React.useEffect(() => {
    if (typeof window === "undefined" || !("speechSynthesis" in window)) return;
    const load = () => {
      const list = window.speechSynthesis
        .getVoices()
        .filter((v) => v.lang.toLowerCase().startsWith("zh"))
        .map((v) => v.name);
      setBrowserVoices(Array.from(new Set(list)).slice(0, 8));
    };
    load();
    window.speechSynthesis.addEventListener?.("voiceschanged", load);
    return () => window.speechSynthesis.removeEventListener?.("voiceschanged", load);
  }, []);

  const httpTtsQuery = useQuery({
    queryKey: ["settings", "httpTts"],
    queryFn: () => featuresApi.listHttpTts(),
  });
  const httpTtsList = httpTtsQuery.data ?? [];

  const saveTtsMutation = useMutation({
    mutationFn: async (d: HttpTtsDraft) => {
      const name = d.name.trim();
      const url = d.url.trim();
      if (!name) throw new Error("请填写音源名称");
      if (!url) throw new Error("请填写请求地址 URL");
      await featuresApi.saveHttpTts({
        id: url,
        url,
        name,
        type: d.type,
        contentType: d.contentType.trim() || null,
        concurrentRate: d.concurrentRate.trim() || null,
        header: d.header.trim() || null,
        loginUrl: d.loginUrl.trim() || null,
        loginUi: d.loginUi.trim() || null,
        jsLib: d.jsLib.trim() || null,
        loginCheckJs: d.loginCheckJs.trim() || null,
        enabledCookieJar: d.enabledCookieJar,
        lastUpdateTime: Date.now(),
      });
    },
    onSuccess: (_data, vars) => {
      notify.success(`已保存音源「${vars.name}」`);
      setTtsDraft(null);
      setTtsDraftBase(null);
      void qc.invalidateQueries({ queryKey: ["settings", "httpTts"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const deleteTtsMutation = useMutation({
    mutationFn: (url: string) => featuresApi.deleteHttpTts(url),
    onSuccess: () => {
      notify.success("已删除 HttpTTS 音源");
      void qc.invalidateQueries({ queryKey: ["settings", "httpTts"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const openTtsDraft = (d: HttpTtsDraft) => {
    setTtsDraft(d);
    setTtsDraftBase(d);
  };

  const openEditTts = (item: HttpTts) => {
    openTtsDraft({
      isNew: false,
      name: item.name,
      url: item.url || item.id,
      type: item.type ?? 0,
      contentType: item.contentType ?? "",
      concurrentRate: item.concurrentRate ?? "",
      header: item.header ?? "",
      loginUrl: item.loginUrl ?? "",
      loginUi: item.loginUi ?? "",
      jsLib: item.jsLib ?? "",
      loginCheckJs: item.loginCheckJs ?? "",
      enabledCookieJar: Boolean(item.enabledCookieJar),
    });
  };

  const ttsDirty = ttsDraftDirty(ttsDraft, ttsDraftBase);

  const forceCloseTts = React.useCallback(() => {
    setTtsDraft(null);
    setTtsDraftBase(null);
    setDiscardTtsAsk(false);
  }, []);

  const requestCloseTts = () => {
    if (saveTtsMutation.isPending) return;
    if (ttsDirty) setDiscardTtsAsk(true);
    else forceCloseTts();
  };

  return (
    <SectionStack>
      {/* Default TTS Preferences */}
      <SettingsSection
        id="tts-prefs"
        title="默认听书偏好"
        nav="朗读偏好"
        desc="设置阅读器内置朗读使用的默认引擎、音色与语速"
      >
        <SettingsCard>
          <CardGroupLabel>引擎与音色</CardGroupLabel>
          <SettingRow
            title="默认朗读引擎"
            desc="浏览器本地语音零延迟，Edge / HttpTTS 音质更自然"
            control={
              <Segmented<TtsEngine>
                layoutId="v2-tts-engine"
                size="sm"
                value={prefs.ttsEngine}
                onChange={prefs.setTtsEngine}
                options={[
                  { value: "browser", label: "浏览器语音" },
                  { value: "edge", label: "后端 Edge TTS" },
                  { value: "http", label: "自定义 HttpTTS" },
                ]}
              />
            }
          />

          <div className="flex flex-col gap-2.5 px-4 py-3.5">
            <Input
              label="默认音色 / 音源名称"
              placeholder={
                prefs.ttsEngine === "edge"
                  ? "留空使用服务端默认，如 zh-CN-XiaoxiaoNeural"
                  : prefs.ttsEngine === "http"
                    ? "填写下方 HttpTTS 音源名称"
                    : "留空使用系统默认中文语音"
              }
              value={prefs.ttsVoice}
              clearable
              onClear={() => prefs.setTtsVoice("")}
              onChange={(e) => prefs.setTtsVoice(e.target.value)}
            />

            {prefs.ttsEngine === "edge" ? (
              <div className="flex flex-wrap gap-1.5">
                {EDGE_VOICE_PRESETS.map((v) => (
                  <button
                    key={v.value}
                    type="button"
                    onClick={() => prefs.setTtsVoice(v.value)}
                    className={cn(
                      "cursor-pointer rounded-full border px-2.5 py-1 text-[11px] transition-colors",
                      prefs.ttsVoice === v.value
                        ? "border-accent bg-accent-soft text-accent"
                        : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong",
                    )}
                  >
                    {v.label}
                  </button>
                ))}
              </div>
            ) : null}

            {prefs.ttsEngine === "browser" && browserVoices.length > 0 ? (
              <div className="flex flex-wrap gap-1.5">
                {browserVoices.map((name) => (
                  <button
                    key={name}
                    type="button"
                    onClick={() => prefs.setTtsVoice(name)}
                    className={cn(
                      "cursor-pointer rounded-full border px-2.5 py-1 text-[11px] transition-colors",
                      prefs.ttsVoice === name
                        ? "border-accent bg-accent-soft text-accent"
                        : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong",
                    )}
                  >
                    {name}
                  </button>
                ))}
              </div>
            ) : null}

            {prefs.ttsEngine === "http" && httpTtsList.length > 0 ? (
              <div className="flex flex-wrap gap-1.5">
                {httpTtsList.map((item) => (
                  <button
                    key={item.url}
                    type="button"
                    onClick={() => prefs.setTtsVoice(item.name)}
                    className={cn(
                      "cursor-pointer rounded-full border px-2.5 py-1 text-[11px] transition-colors",
                      prefs.ttsVoice === item.name
                        ? "border-accent bg-accent-soft text-accent"
                        : "border-line bg-subtle/60 text-ink-secondary hover:border-line-strong",
                    )}
                  >
                    {item.name}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        </SettingsCard>

        <SettingsCard>
          <CardGroupLabel>播放控制</CardGroupLabel>
          <div className="px-4 py-4">
            <div className="mb-1.5 flex items-center justify-between text-xs">
              <span className="font-medium text-ink-secondary">默认朗读倍速</span>
              <span className="font-mono text-ink tabular-nums">{prefs.ttsRate.toFixed(1)}×</span>
            </div>
            <Slider
              ariaLabel="默认朗读倍速"
              min={0.6}
              max={2.0}
              step={0.1}
              value={prefs.ttsRate}
              onChange={prefs.setTtsRate}
            />
          </div>

          <SettingRow
            title="章节结束自动连播"
            desc="当前章节朗读完成后自动翻至下一章继续播放"
            control={
              <Switch
                checked={prefs.ttsAutoNext}
                ariaLabel="章节结束自动连播"
                onChange={prefs.setTtsAutoNext}
              />
            }
          />
        </SettingsCard>
      </SettingsSection>

      {/* HttpTTS Sources */}
      <SettingsSection
        id="http-tts"
        title="HttpTTS 自定义音源"
        nav="HttpTTS 音源"
        badge={<Badge>{httpTtsList.length}</Badge>}
        desc="兼容 Legado HttpTTS 规范，通过模板 URL 接入自建或第三方语音合成接口"
        actions={
          <>
            <IconButton
              size="sm"
              aria-label="刷新音源列表"
              onClick={() => void httpTtsQuery.refetch()}
            >
              <RefreshCw className={cn("size-3.5", httpTtsQuery.isFetching && "animate-spin")} />
            </IconButton>
            <Button
              variant="primary"
              size="sm"
              onClick={() => openTtsDraft({ ...EMPTY_TTS_DRAFT })}
            >
              <Plus className="size-3.5" />
              新增音源
            </Button>
          </>
        }
      >
        {httpTtsQuery.isLoading ? (
          <div className="flex justify-center py-8">
            <Spinner label="加载 HttpTTS 音源…" />
          </div>
        ) : httpTtsList.length === 0 ? (
          <div className="rounded-2xl border border-dashed border-line-strong bg-subtle/25">
            <EmptyView
              compact
              icon={<Volume2 className="size-5" />}
              title="暂无自定义 HttpTTS 音源"
              description="点击「新增音源」接入在线语音合成服务"
            />
          </div>
        ) : (
          <SettingsCard>
            {httpTtsList.map((item) => {
              const isCurrent = prefs.ttsEngine === "http" && prefs.ttsVoice === item.name;
              return (
                <div
                  key={item.url}
                  className="flex flex-col gap-2.5 px-4 py-3.5 transition-colors hover:bg-subtle/30 sm:flex-row sm:items-center sm:justify-between"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-1.5">
                      <span className="truncate text-[13px] font-medium text-ink">{item.name}</span>
                      <Badge>{item.type === 1 ? "本地引擎" : "在线合成"}</Badge>
                      {item.contentType ? <Badge>{item.contentType}</Badge> : null}
                      {isCurrent ? <Badge tone="accent">当前默认</Badge> : null}
                    </div>
                    <p className="mt-1 truncate font-mono text-[11px] text-ink-muted">
                      {item.url}
                    </p>
                  </div>
                  <div className="flex shrink-0 items-center gap-1 self-end sm:self-center">
                    {!isCurrent ? (
                      <Button
                        variant="subtle"
                        size="xs"
                        onClick={() => {
                          prefs.setTtsEngine("http");
                          prefs.setTtsVoice(item.name);
                          notify.success(`已设「${item.name}」为默认听书音源`);
                        }}
                      >
                        设为默认
                      </Button>
                    ) : null}
                    <Button variant="ghost" size="xs" onClick={() => openEditTts(item)}>
                      <Pencil className="size-3" />
                      编辑
                    </Button>
                    <ConfirmAction
                      title={`删除音源「${item.name}」？`}
                      description="删除后该 HttpTTS 配置将从服务端永久移除。"
                      consequences={[
                        "音源 URL 模板、请求头与脚本配置将一并清除，无法恢复",
                        ...(isCurrent
                          ? ["该音源正被设为默认听书音源，删除后请在上方重新选择音色"]
                          : []),
                      ]}
                      confirmLabel="确认删除"
                      icon={<Trash2 className="size-[18px]" />}
                      busy={deleteTtsMutation.isPending}
                      onConfirm={() => deleteTtsMutation.mutateAsync(item.url)}
                      trigger={(open) => (
                        <IconButton
                          size="sm"
                          tone="danger"
                          aria-label={`删除音源 ${item.name}`}
                          onClick={open}
                        >
                          <Trash2 className="size-3.5" />
                        </IconButton>
                      )}
                    />
                  </div>
                </div>
              );
            })}
          </SettingsCard>
        )}
      </SettingsSection>

      {/* Add / Edit HttpTTS Sheet */}
      <Sheet
        open={ttsDraft !== null}
        onClose={requestCloseTts}
        size="md"
        title={ttsDraft?.isNew ? "新增 HttpTTS 音源" : `编辑音源 · ${ttsDraft?.name ?? ""}`}
        subtitle="支持 {{speakText}} 与 {{speakSpeed}} 模板变量"
        footer={
          ttsDraft ? (
            <div className="flex items-center justify-between gap-3">
              <span className="text-[11px] text-ink-muted">
                {saveTtsMutation.isPending
                  ? "正在保存…"
                  : ttsDirty
                    ? "有未保存的更改"
                    : ttsDraft.isNew
                      ? "填写必填项后保存"
                      : "无未保存更改"}
              </span>
              <div className="flex items-center gap-2">
                <Button variant="ghost" size="sm" onClick={requestCloseTts} disabled={saveTtsMutation.isPending}>
                  取消
                </Button>
                <Button
                  variant="primary"
                  size="sm"
                  loading={saveTtsMutation.isPending}
                  onClick={() => saveTtsMutation.mutate(ttsDraft)}
                >
                  保存音源
                </Button>
              </div>
            </div>
          ) : null
        }
      >
        {ttsDraft ? (
          <div className="flex flex-col gap-5">
            <div className="flex flex-col gap-3">
              <p className="text-[11px] font-semibold tracking-wider text-ink-muted uppercase">基础配置</p>
              <Input
                label="音源名称 *"
                placeholder="例如：自建 Azure TTS"
                value={ttsDraft.name}
                onChange={(e) => setTtsDraft({ ...ttsDraft, name: e.target.value })}
              />
              <Textarea
                label="合成请求 URL *"
                mono
                rows={3}
                placeholder="https://tts.example.com/api?text={{java.encodeURI(speakText)}}&speed={{speakSpeed}}"
                hint="作为唯一标识，编辑时修改 URL 将另存为新条目"
                value={ttsDraft.url}
                onChange={(e) => setTtsDraft({ ...ttsDraft, url: e.target.value })}
              />
              <div className="flex items-center justify-between gap-3 rounded-xl border border-line bg-elevated px-3.5 py-2.5">
                <span className="text-xs font-medium text-ink-secondary">引擎类型</span>
                <Segmented<"0" | "1">
                  layoutId="v2-httptts-type"
                  size="sm"
                  value={String(ttsDraft.type) as "0" | "1"}
                  onChange={(v) => setTtsDraft({ ...ttsDraft, type: Number(v) })}
                  options={[
                    { value: "0", label: "在线合成" },
                    { value: "1", label: "本地引擎" },
                  ]}
                />
              </div>
            </div>

            <div className="flex flex-col gap-3 border-t border-line pt-4">
              <p className="text-[11px] font-semibold tracking-wider text-ink-muted uppercase">请求与会话</p>
              <div className="grid grid-cols-2 gap-3">
                <Input
                  label="响应 MIME"
                  placeholder="audio/mpeg"
                  value={ttsDraft.contentType}
                  onChange={(e) => setTtsDraft({ ...ttsDraft, contentType: e.target.value })}
                />
                <Input
                  label="并发限制"
                  placeholder="如 1/1000"
                  value={ttsDraft.concurrentRate}
                  onChange={(e) => setTtsDraft({ ...ttsDraft, concurrentRate: e.target.value })}
                />
              </div>
              <Textarea
                label="请求头 Header (JSON)"
                mono
                rows={3}
                placeholder='{"Authorization": "Bearer ..."}'
                value={ttsDraft.header}
                onChange={(e) => setTtsDraft({ ...ttsDraft, header: e.target.value })}
              />
              <div className="flex items-center justify-between rounded-xl border border-line bg-elevated px-3.5 py-2.5">
                <div>
                  <p className="text-xs font-medium text-ink">启用 CookieJar</p>
                  <p className="text-[11px] text-ink-muted">在连续合成请求间保持会话 Cookie</p>
                </div>
                <Switch
                  checked={ttsDraft.enabledCookieJar}
                  ariaLabel="启用 CookieJar"
                  onChange={(v) => setTtsDraft({ ...ttsDraft, enabledCookieJar: v })}
                />
              </div>
            </div>

            <div className="flex flex-col gap-3 border-t border-line pt-4">
              <p className="text-[11px] font-semibold tracking-wider text-ink-muted uppercase">登录与脚本（可选）</p>
              <Input
                label="登录 URL（可选）"
                placeholder="https://..."
                value={ttsDraft.loginUrl}
                onChange={(e) => setTtsDraft({ ...ttsDraft, loginUrl: e.target.value })}
              />
              <Textarea
                label="JS 依赖库（可选）"
                mono
                rows={2}
                value={ttsDraft.jsLib}
                onChange={(e) => setTtsDraft({ ...ttsDraft, jsLib: e.target.value })}
              />
            </div>
          </div>
        ) : null}
      </Sheet>

      <ConfirmDialog
        open={discardTtsAsk}
        onClose={() => setDiscardTtsAsk(false)}
        tone="warn"
        title="放弃未保存的音源配置？"
        description="关闭后已填写或修改的 HttpTTS 参数将不会保存。"
        cancelLabel="继续编辑"
        confirmLabel="放弃更改"
        onConfirm={forceCloseTts}
      />
    </SectionStack>
  );
}

/* ==========================================================================
   3. Cache & WebDAV Backup Files
   ========================================================================== */

function StorageSection() {
  const qc = useQueryClient();
  const system = useAuthStore((s) => s.system);
  const user = useAuthStore((s) => s.user);
  const uploadRef = React.useRef<HTMLInputElement>(null);
  const [currentPath, setCurrentPath] = React.useState("/");

  const webdavAllowed = !system?.secure || Boolean(user?.enableWebdav);

  const cacheQuery = useQuery({
    queryKey: ["settings", "cacheInfo"],
    queryFn: () => featuresApi.getCacheInfo(),
  });

  const filesQuery = useQuery({
    queryKey: ["settings", "webdavFiles", currentPath],
    queryFn: () => featuresApi.listFiles("__WEBDAV__", currentPath),
    enabled: webdavAllowed,
  });

  const clearCacheMutation = useMutation({
    mutationFn: (type: "toc" | "chapters" | "all") => featuresApi.clearCache(type),
    onSuccess: (res, type) => {
      const label = type === "toc" ? "目录缓存" : type === "chapters" ? "正文缓存" : "全部缓存";
      notify.success(
        `已清理${label}（目录 ${res.deletedToc} 项 · 正文 ${res.deletedChapters} 章）`,
      );
      void qc.invalidateQueries({ queryKey: ["settings", "cacheInfo"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const backupMutation = useMutation({
    mutationFn: () => featuresApi.backupToWebdav(),
    onSuccess: (res) => {
      notify.success(`备份完成：${res.path}`);
      void qc.invalidateQueries({ queryKey: ["settings", "webdavFiles"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const uploadFilesMutation = useMutation({
    mutationFn: (files: File[]) => featuresApi.uploadFiles(files, "__WEBDAV__", currentPath),
    onSuccess: (uploaded) => {
      notify.success(`已上传 ${uploaded.length} 个备份文件`);
      void qc.invalidateQueries({ queryKey: ["settings", "webdavFiles"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const restoreMutation = useMutation({
    mutationFn: ({ path, overwrite }: { path: string; overwrite: boolean }) =>
      featuresApi.restoreFromWebdav(path, overwrite),
    onSuccess: (report) => {
      notify.success(summarizeRestore(report));
      void qc.invalidateQueries();
    },
    onError: (err) => notify.error(errText(err)),
  });

  const deleteFileMutation = useMutation({
    mutationFn: (path: string) => featuresApi.deleteFile(path, "__WEBDAV__"),
    onSuccess: () => {
      notify.success("已删除备份文件");
      void qc.invalidateQueries({ queryKey: ["settings", "webdavFiles"] });
    },
    onError: (err) => notify.error(errText(err)),
  });

  const cache = cacheQuery.data;
  const totalBytes = Math.max(0, cache?.totalSize ?? 0);
  const tocRatio = totalBytes > 0 ? Math.min(100, ((cache?.tocCacheSize ?? 0) / totalBytes) * 100) : 0;
  const chapterRatio =
    totalBytes > 0 ? Math.min(100, ((cache?.chapterSize ?? 0) / totalBytes) * 100) : 0;

  const sortedFiles = React.useMemo(
    () =>
      [...(filesQuery.data ?? [])].sort((a, b) => {
        if (a.isDirectory !== b.isDirectory) return a.isDirectory ? -1 : 1;
        return (b.lastModified || 0) - (a.lastModified || 0);
      }),
    [filesQuery.data],
  );

  const breadcrumbs = React.useMemo(() => {
    const norm = normalizeRemotePath(currentPath);
    if (norm === "/") return [{ label: "根目录", path: "/" }];
    const segments = norm.split("/").filter(Boolean);
    return [
      { label: "根目录", path: "/" },
      ...segments.map((seg, i) => ({
        label: seg,
        path: `/${segments.slice(0, i + 1).join("/")}`,
      })),
    ];
  }, [currentPath]);

  return (
    <SectionStack>
      {/* Server Cache Card */}
      <SettingsSection
        id="cache"
        title="服务端缓存"
        nav="服务端缓存"
        desc="目录与章节正文均缓存于服务端，清理后重新阅读时将自动重新抓取"
        actions={
          <IconButton
            size="sm"
            aria-label="刷新缓存统计"
            onClick={() => void cacheQuery.refetch()}
          >
            <RefreshCw className={cn("size-3.5", cacheQuery.isFetching && "animate-spin")} />
          </IconButton>
        }
      >
        <SettingsCard>
          {cacheQuery.isLoading ? (
            <div className="flex justify-center py-8">
              <Spinner label="统计缓存占用…" />
            </div>
          ) : (
            <>
              <div className="flex flex-col gap-4 p-5">
                <div className="grid grid-cols-1 gap-3 tabular-nums sm:grid-cols-3">
                  <div className="rounded-xl bg-subtle/65 px-3.5 py-3">
                    <p className="text-[11px] text-ink-muted">目录缓存</p>
                    <p className="mt-1 font-serif text-lg font-semibold text-ink">
                      {formatBytes(cache?.tocCacheSize ?? 0)}
                    </p>
                    <p className="mt-0.5 text-[11px] text-ink-secondary">
                      {cache?.tocCacheCount ?? 0} 本目录
                    </p>
                  </div>

                  <div className="rounded-xl bg-subtle/65 px-3.5 py-3">
                    <p className="text-[11px] text-ink-muted">正文缓存</p>
                    <p className="mt-1 font-serif text-lg font-semibold text-ink">
                      {formatBytes(cache?.chapterSize ?? 0)}
                    </p>
                    <p className="mt-0.5 text-[11px] text-ink-secondary">
                      {cache?.chapterCount ?? 0} 章节
                    </p>
                  </div>

                  <div className="rounded-xl bg-accent-soft/55 px-3.5 py-3">
                    <p className="text-[11px] text-accent">总计占用</p>
                    <p className="mt-1 font-serif text-lg font-semibold text-ink">
                      {formatBytes(totalBytes)}
                    </p>
                    <p className="mt-0.5 text-[11px] text-ink-secondary">SQLite 缓存仓</p>
                  </div>
                </div>

                {/* Proportional bar */}
                <div className="flex flex-col gap-1.5">
                  <div className="flex h-2.5 w-full overflow-hidden rounded-full bg-subtle">
                    {tocRatio > 0 ? (
                      <div
                        style={{ width: `${Math.max(2, tocRatio)}%` }}
                        className="h-full bg-warn transition-all duration-300"
                        title={`目录缓存 ${formatBytes(cache?.tocCacheSize ?? 0)}`}
                      />
                    ) : null}
                    {chapterRatio > 0 ? (
                      <div
                        style={{ width: `${Math.max(2, chapterRatio)}%` }}
                        className="h-full bg-accent transition-all duration-300"
                        title={`正文缓存 ${formatBytes(cache?.chapterSize ?? 0)}`}
                      />
                    ) : null}
                  </div>
                  <div className="flex items-center gap-4 text-[11px] text-ink-muted">
                    <span className="inline-flex items-center gap-1.5">
                      <span className="size-2 rounded-full bg-warn" />
                      目录缓存
                    </span>
                    <span className="inline-flex items-center gap-1.5">
                      <span className="size-2 rounded-full bg-accent" />
                      章节正文
                    </span>
                  </div>
                </div>
              </div>

              <div className="flex flex-wrap items-center justify-between gap-2 bg-subtle/25 px-5 py-3.5">
                <div className="flex flex-wrap items-center gap-2">
                  <ConfirmAction
                    tone="warn"
                    title="清理目录缓存？"
                    description="仅清空服务端缓存的章节目录索引，不影响书架与阅读进度。"
                    consequences={[
                      `预计释放 ${formatBytes(cache?.tocCacheSize ?? 0)}（${cache?.tocCacheCount ?? 0} 本目录）`,
                      "下次打开目录时将向书源重新拉取最新章节列表",
                    ]}
                    confirmLabel="确认清理目录"
                    busy={clearCacheMutation.isPending}
                    onConfirm={() => clearCacheMutation.mutateAsync("toc")}
                    trigger={(open) => (
                      <Button variant="secondary" size="sm" onClick={open}>
                        清理目录缓存
                      </Button>
                    )}
                  />
                  <ConfirmAction
                    tone="warn"
                    title="清理章节正文缓存？"
                    description="移除服务端已下载的章节文本以释放空间，保留目录与阅读进度。"
                    consequences={[
                      `预计释放 ${formatBytes(cache?.chapterSize ?? 0)}（${cache?.chapterCount ?? 0} 章节）`,
                      "再次阅读相关章节时需要联网重新获取正文",
                    ]}
                    confirmLabel="确认清理正文"
                    busy={clearCacheMutation.isPending}
                    onConfirm={() => clearCacheMutation.mutateAsync("chapters")}
                    trigger={(open) => (
                      <Button variant="secondary" size="sm" onClick={open}>
                        清理正文缓存
                      </Button>
                    )}
                  />
                </div>

                <ConfirmAction
                  tone="danger"
                  icon={<Trash2 className="size-[18px]" />}
                  title="清空全部服务端缓存？"
                  description="一次性清空所有目录与章节正文缓存，不影响个人书架与配置。"
                  consequences={[
                    `将释放约 ${formatBytes(totalBytes)}（${cache?.tocCacheCount ?? 0} 本目录 · ${cache?.chapterCount ?? 0} 章节）`,
                    "书架、书源、书签与阅读进度会被完整保留",
                    "重新阅读此前已缓存的书籍时将自动联网重新拉取",
                  ]}
                  confirmLabel="清空全部缓存"
                  busy={clearCacheMutation.isPending}
                  onConfirm={() => clearCacheMutation.mutateAsync("all")}
                  trigger={(open) => (
                    <Button variant="danger" size="sm" onClick={open}>
                      <Trash2 className="size-3.5" />
                      清空全部缓存
                    </Button>
                  )}
                />
              </div>
            </>
          )}
        </SettingsCard>
      </SettingsSection>

      {/* WebDAV Backup & Remote File Browser */}
      <SettingsSection
        id="webdav"
        title="WebDAV 备份与文件仓"
        nav="WebDAV 备份"
        badge={webdavAllowed && sortedFiles.length > 0 ? <Badge>{sortedFiles.length} 项</Badge> : undefined}
        desc="一键打包书架、书源、书签、分组与净化规则为 ZIP，支持浏览远端目录与增量 / 覆盖恢复"
        actions={
          webdavAllowed ? (
            <>
              <IconButton
                size="sm"
                aria-label="刷新文件列表"
                onClick={() => void filesQuery.refetch()}
              >
                <RefreshCw className={cn("size-3.5", filesQuery.isFetching && "animate-spin")} />
              </IconButton>
              <Button
                variant="secondary"
                size="sm"
                loading={uploadFilesMutation.isPending}
                onClick={() => uploadRef.current?.click()}
              >
                <Upload className="size-3.5" />
                上传备份包
              </Button>
              <input
                ref={uploadRef}
                type="file"
                multiple
                accept=".zip,.json"
                className="hidden"
                onChange={(e) => {
                  const files = Array.from(e.target.files ?? []);
                  if (files.length > 0) uploadFilesMutation.mutate(files);
                  e.target.value = "";
                }}
              />
              <Button
                variant="primary"
                size="sm"
                loading={backupMutation.isPending}
                onClick={() => backupMutation.mutate()}
              >
                <Archive className="size-3.5" />
                立即备份
              </Button>
            </>
          ) : undefined
        }
      >
        {!webdavAllowed ? (
          <div className="rounded-2xl border border-dashed border-line-strong bg-subtle/25">
            <EmptyView
              compact
              icon={<HardDrive className="size-5" />}
              title="WebDAV 备份权限未开启"
              description="当前账号未开通 WebDAV 权限，请联系管理员在账号管理中开启"
            />
          </div>
        ) : (
          <>
            {/* Remote path breadcrumb bar when inside subdirectory */}
            {currentPath !== "/" ? (
              <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-line bg-subtle/40 px-3 py-2 text-xs">
                <nav aria-label="远端目录路径" className="flex min-w-0 flex-wrap items-center gap-1">
                  {breadcrumbs.map((b, idx) => {
                    const last = idx === breadcrumbs.length - 1;
                    return (
                      <React.Fragment key={b.path}>
                        {idx > 0 ? <ChevronRight className="size-3 text-ink-muted" /> : null}
                        <button
                          type="button"
                          disabled={last}
                          onClick={() => setCurrentPath(b.path)}
                          className={cn(
                            "max-w-[160px] truncate rounded-md px-1.5 py-0.5 transition-colors",
                            last
                              ? "font-medium text-ink"
                              : "cursor-pointer text-ink-muted hover:bg-subtle hover:text-ink",
                          )}
                        >
                          {b.label}
                        </button>
                      </React.Fragment>
                    );
                  })}
                </nav>
                <Button
                  variant="ghost"
                  size="xs"
                  onClick={() => setCurrentPath(parentRemotePath(currentPath))}
                >
                  <ArrowUp className="size-3" />
                  上一级
                </Button>
              </div>
            ) : null}

            {filesQuery.isLoading ? (
              <div className="flex justify-center py-8">
                <Spinner label="读取备份文件仓…" />
              </div>
            ) : sortedFiles.length === 0 ? (
              <div className="rounded-2xl border border-dashed border-line-strong bg-subtle/25">
                <EmptyView
                  compact
                  icon={<FileArchive className="size-5" />}
                  title={currentPath === "/" ? "备份文件仓为空" : "当前目录为空"}
                  description={
                    currentPath === "/"
                      ? "点击「立即备份」生成当前数据的完整快照，或上传已有的备份 .zip"
                      : "可上传文件至此目录，或返回上一级查看其他备份"
                  }
                  action={
                    currentPath !== "/" ? (
                      <Button variant="ghost" size="xs" onClick={() => setCurrentPath("/")}>
                        返回根目录
                      </Button>
                    ) : undefined
                  }
                />
              </div>
            ) : (
              <SettingsCard>
                {sortedFiles.map((entry) => {
                  const isZip = !entry.isDirectory && entry.name.toLowerCase().endsWith(".zip");
                  return (
                    <div
                      key={entry.path}
                      className="flex flex-col gap-2.5 px-4 py-3.5 transition-colors hover:bg-subtle/30 sm:flex-row sm:items-center sm:justify-between"
                    >
                      {entry.isDirectory ? (
                        <button
                          type="button"
                          onClick={() => setCurrentPath(normalizeRemotePath(entry.path))}
                          className="flex min-w-0 flex-1 cursor-pointer items-center gap-3 text-left outline-none"
                        >
                          <div className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-accent-soft/65 text-accent">
                            <Folder className="size-4" />
                          </div>
                          <div className="min-w-0 flex-1">
                            <div className="flex items-center gap-1.5">
                              <p className="truncate text-[13px] font-medium text-ink">{entry.name}</p>
                              <ChevronRight className="size-3.5 shrink-0 text-ink-muted" />
                            </div>
                            <p className="mt-0.5 text-[11px] text-ink-muted tabular-nums">
                              目录{entry.lastModified ? ` · ${formatRelativeTime(entry.lastModified)}` : ""}
                            </p>
                          </div>
                        </button>
                      ) : (
                        <div className="flex min-w-0 flex-1 items-center gap-3">
                          <div className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-subtle text-ink-secondary">
                            {isZip ? <FileArchive className="size-4 text-accent" /> : <File className="size-4" />}
                          </div>
                          <div className="min-w-0">
                            <p className="truncate text-[13px] font-medium text-ink">{entry.name}</p>
                            <p className="mt-0.5 text-[11px] text-ink-muted tabular-nums">
                              {formatBytes(entry.size)}
                              {entry.lastModified ? ` · ${formatRelativeTime(entry.lastModified)}` : ""}
                            </p>
                          </div>
                        </div>
                      )}

                      <div className="flex shrink-0 flex-wrap items-center gap-1.5 self-end sm:self-center">
                        {isZip ? (
                          <>
                            <ConfirmAction
                              tone="accent"
                              icon={<History className="size-[18px]" />}
                              title={`增量恢复「${entry.name}」？`}
                              description="仅补齐当前缺失的数据条目，已有的书籍与规则保持不变。"
                              consequences={[
                                "自动导入备份包中新增的书籍、书源、书签、分组、净化规则与 HttpTTS 音源",
                                "已存在的同名条目会被跳过，不会覆盖当前阅读进度",
                              ]}
                              confirmLabel="开始增量恢复"
                              busy={restoreMutation.isPending}
                              onConfirm={() =>
                                restoreMutation.mutateAsync({ path: entry.path, overwrite: false })
                              }
                              trigger={(open) => (
                                <Button variant="subtle" size="xs" onClick={open}>
                                  增量恢复
                                </Button>
                              )}
                            />
                            <ConfirmAction
                              tone="danger"
                              icon={<DatabaseZap className="size-[18px]" />}
                              title={`覆盖恢复「${entry.name}」？`}
                              description="高风险操作：将使用该备份包覆盖当前同名数据，建议先执行「立即备份」。"
                              consequences={[
                                "同名书籍的阅读进度、书源、书签、分组、净化规则将被备份包内旧数据替换",
                                "覆盖后立即生效且无法自动撤销",
                              ]}
                              confirmLabel="确认覆盖恢复"
                              busy={restoreMutation.isPending}
                              onConfirm={() =>
                                restoreMutation.mutateAsync({ path: entry.path, overwrite: true })
                              }
                              trigger={(open) => (
                                <Button variant="ghost" size="xs" onClick={open}>
                                  覆盖恢复
                                </Button>
                              )}
                            />
                          </>
                        ) : null}

                        {!entry.isDirectory ? (
                          <a
                            href={fileDownloadUrl("__WEBDAV__", entry.path)}
                            download={entry.name}
                            aria-label={`下载 ${entry.name}`}
                            className="inline-flex size-8 items-center justify-center rounded-full text-ink-secondary transition-colors hover:bg-subtle hover:text-ink"
                          >
                            <Download className="size-3.5" />
                          </a>
                        ) : null}

                        <ConfirmAction
                          tone="danger"
                          icon={<Trash2 className="size-[18px]" />}
                          title={`删除${entry.isDirectory ? "目录" : "备份文件"}「${entry.name}」？`}
                          description="该项将从 WebDAV 远端文件仓永久移除。"
                          consequences={
                            entry.isDirectory
                              ? ["该目录及其包含的所有文件将被永久删除，无法找回"]
                              : [
                                  `文件大小 ${formatBytes(entry.size)}，删除后无法恢复`,
                                  "不影响当前已保存在服务端的书架与配置",
                                ]
                          }
                          confirmLabel="确认删除"
                          busy={deleteFileMutation.isPending}
                          onConfirm={() => deleteFileMutation.mutateAsync(entry.path)}
                          trigger={(open) => (
                            <IconButton
                              size="sm"
                              tone="danger"
                              aria-label={`删除 ${entry.name}`}
                              onClick={open}
                            >
                              <Trash2 className="size-3.5" />
                            </IconButton>
                          )}
                        />
                      </div>
                    </div>
                  );
                })}
              </SettingsCard>
            )}
          </>
        )}
      </SettingsSection>
    </SectionStack>
  );
}

/* ==========================================================================
   4. Account, System Info & Embedded UsersSection
   ========================================================================== */

function AccountSection() {
  const navigate = useNavigate();
  const system = useAuthStore((s) => s.system);
  const user = useAuthStore((s) => s.user);
  const logout = useAuthStore((s) => s.logout);
  const [loggingOut, setLoggingOut] = React.useState(false);

  const isMulti = Boolean(system?.secure || system?.mode === "multi");
  const isAdmin = Boolean(system?.isAdmin || user?.isAdmin);

  const handleLogout = async () => {
    setLoggingOut(true);
    try {
      await logout();
      notify.info("已退出登录");
      navigate("/login", { replace: true });
    } finally {
      setLoggingOut(false);
    }
  };

  return (
    <SectionStack>
      {/* System Overview */}
      <SettingsSection
        id="overview"
        title="系统概览"
        nav="系统概览"
        desc="当前服务端实例的运行模式、版本与在线时长"
      >
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 tabular-nums">
          <div className="rounded-2xl border border-line bg-elevated px-4 py-3.5">
            <div className="flex items-center gap-1.5 text-[11px] text-ink-muted">
              <Server className="size-3.5" />
              运行模式
            </div>
            <p className="mt-1.5 text-sm font-semibold text-ink">
              {isMulti ? "多用户鉴权" : "单用户模式"}
            </p>
          </div>

          <div className="rounded-2xl border border-line bg-elevated px-4 py-3.5">
            <div className="flex items-center gap-1.5 text-[11px] text-ink-muted">
              <ShieldCheck className="size-3.5" />
              服务端版本
            </div>
            <p className="mt-1.5 font-mono text-sm font-semibold text-ink">
              v{system?.version ?? "—"}
            </p>
          </div>

          <div className="rounded-2xl border border-line bg-elevated px-4 py-3.5">
            <div className="flex items-center gap-1.5 text-[11px] text-ink-muted">
              <Clock className="size-3.5" />
              连续运行时长
            </div>
            <p className="mt-1.5 text-sm font-semibold text-ink">
              {formatUptime(system?.uptimeSeconds)}
            </p>
          </div>

          <div className="rounded-2xl border border-line bg-elevated px-4 py-3.5">
            <div className="flex items-center gap-1.5 text-[11px] text-ink-muted">
              <UserCog className="size-3.5" />
              注册上限
            </div>
            <p className="mt-1.5 text-sm font-semibold text-ink">
              {isMulti ? (system && system.userLimit > 0 ? `${system.userLimit} 人` : "不限") : "单机"}
            </p>
          </div>
        </div>
      </SettingsSection>

      {/* Current User Card */}
      <SettingsSection
        id="profile"
        title="当前账号"
        nav="当前账号"
        desc="查看当前登录身份、功能权限与个人配额"
      >
        <SettingsCard>
          <div className="p-5">
            {isMulti && user ? (
              <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
                <div className="flex items-start gap-3.5">
                  <div className="flex size-11 shrink-0 items-center justify-center rounded-full bg-accent-soft font-serif text-base font-semibold text-accent uppercase">
                    {user.username.slice(0, 1)}
                  </div>
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <h3 className="text-base font-semibold text-ink">{user.username}</h3>
                      <Badge tone={user.isAdmin ? "accent" : "default"}>
                        {user.isAdmin ? "管理员" : "普通用户"}
                      </Badge>
                    </div>
                    <p className="mt-1 text-xs text-ink-muted tabular-nums">
                      创建于 {formatRelativeTime(user.createdAt) || "—"} · 最近登录{" "}
                      {formatRelativeTime(user.lastLoginAt) || "刚刚"}
                    </p>
                    <div className="mt-2.5 flex flex-wrap items-center gap-1.5">
                      <Badge tone={user.enableBookSource ? "success" : "warn"}>
                        书源 {user.enableBookSource ? "已开" : "关闭"}
                      </Badge>
                      <Badge tone={user.enableWebdav ? "success" : "warn"}>
                        WebDAV {user.enableWebdav ? "已开" : "关闭"}
                      </Badge>
                      <Badge tone={user.enableLocalStore ? "success" : "warn"}>
                        本地仓 {user.enableLocalStore ? "已开" : "关闭"}
                      </Badge>
                      <Badge>书籍上限 {user.bookLimit > 0 ? user.bookLimit : "不限"}</Badge>
                      <Badge>书源上限 {user.bookSourceLimit > 0 ? user.bookSourceLimit : "不限"}</Badge>
                    </div>
                  </div>
                </div>

                <ConfirmAction
                  tone="warn"
                  icon={<LogOut className="size-[18px]" />}
                  title="确认退出当前账号？"
                  description={`退出 ${user.username} 后将返回登录页。`}
                  consequences={[
                    "当前浏览器的本地阅读排版偏好会继续保留",
                    "需重新登录才能访问个人书架与云端同步数据",
                  ]}
                  confirmLabel="退出登录"
                  busy={loggingOut}
                  onConfirm={handleLogout}
                  trigger={(open) => (
                    <Button
                      variant="secondary"
                      size="sm"
                      loading={loggingOut}
                      onClick={open}
                      className="self-start sm:self-center"
                    >
                      <LogOut className="size-3.5" />
                      退出登录
                    </Button>
                  )}
                />
              </div>
            ) : (
              <div className="flex items-start gap-3.5">
                <div className="flex size-10 shrink-0 items-center justify-center rounded-2xl bg-subtle text-accent">
                  <ShieldCheck className="size-5" />
                </div>
                <div>
                  <p className="text-sm font-semibold text-ink">当前处于单用户免登模式</p>
                  <p className="mt-1 text-xs leading-relaxed text-ink-muted">
                    服务端运行在 `default` 命名空间，已自动授予全部阅读、书源、WebDAV 与系统管理权限。如需多人隔离与账号鉴权，请在服务端配置开启多用户安全模式。
                  </p>
                </div>
              </div>
            )}
          </div>
        </SettingsCard>
      </SettingsSection>

      {/* Seamless Admin Users Management */}
      {isMulti && isAdmin ? <UsersSection /> : null}
    </SectionStack>
  );
}
