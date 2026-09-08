import {
  CircleAlert,
  Database,
  Download,
  RotateCw,
  Search,
  ShieldAlert,
  ToolCase,
  Trash2,
  Upload,
} from "lucide-react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Ellipsis } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui";
import * as React from "react";
import { useNavigate } from "react-router-dom";

import { DebugSourceDrawer } from "@/components/sources/DebugSourceDrawer";
import { EditSourceDialog } from "@/components/sources/EditSourceDialog";
import { ImportSourcesDialog } from "@/components/sources/ImportSourcesDialog";
import { InvalidSourcesDialog } from "@/components/sources/InvalidSourcesDialog";
import { SourceGroupTabs } from "@/components/sources/SourceGroupTabs";
import { SourceListItem } from "@/components/sources/SourceListItem";
import { SourceLoginDialog } from "@/components/sources/SourceLoginDialog";
import {
  SOURCE_GROUP_ALL,
  buildSourceGroups,
  filterSources,
  sourceErrorMessage,
  useBookSources,
  useDeleteBookSources,
  useInvalidBookSources,
  useSourceStats,
  useToggleSourceEnabled,
} from "@/components/sources/useSources";
import { getBookSourceCookie, saveBookSources, type SourceStat } from "@/services/sources";
import { SOURCES_QUERY_KEY } from "@/components/sources/useSources";
import { InvalidReasonDialog } from "@/components/sources/InvalidReasonDialog";
import type { InvalidBookSource } from "@/services/sources";
import { SOURCE_STATS_QUERY_KEY } from "@/components/sources/useSources";
import {
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
  SkeletonList,
  cn,
} from "@/components/ui";
import { toast } from "@/components/ui/Toast";
import type { BookSource } from "@/types/api";

/** 书源列表容器 id: 分组标签通过 aria-controls 指向它 */
const SOURCE_COOKIES_QUERY_KEY = ["sourceCookies"] as const;
const LIST_ID = "sources-list";

/** 书源管理页: 列表(启用开关/编辑/调试/删除) + 分组 Tabs + 本地搜索 + 导入 + 失效检测 */
export default function SourcesPage() {
  const navigate = useNavigate();
  const { sources, isLoading, isFetching, error, refetch } = useBookSources();
  const toggleEnabled = useToggleSourceEnabled();
  const deleteSources = useDeleteBookSources();
  const invalid = useInvalidBookSources();
  const statsQuery = useSourceStats();
  const queryClient = useQueryClient();
  const [cleanupOpen, setCleanupOpen] = React.useState(false);
  const statMap = React.useMemo(() => {
    const map = new Map<string, SourceStat>();
    for (const stat of statsQuery.data ?? []) {
      map.set(stat.sourceUrl, stat);
    }
    return map;
  }, [statsQuery.data]);
  /** 清理目标: 尝试≥5 且成功率<20% 的启用源 (置信度已被服务端压到 0.05 档) */
  /** 长期统计失效 url 集: 与清理目标同口径(含已禁用源, 供徽标) */
  const deadUrls = React.useMemo(
    () =>
      new Set(
        sources
          .filter((source) => {
            const stat = statMap.get(source.bookSourceUrl);
            return stat !== undefined && stat.attempts >= 5 && stat.successRate < 0.2;
          })
          .map((source) => source.bookSourceUrl),
      ),
    [sources, statMap],
  );
  const cleanupTargets = React.useMemo(
    () =>
      sources.filter((source) => {
        const stat = statMap.get(source.bookSourceUrl);
        return source.enabled && stat !== undefined && stat.attempts >= 5 && stat.successRate < 0.2;
      }),
    [sources, statMap],
  );
  const cleanup = useMutation({
    mutationFn: () => saveBookSources(cleanupTargets.map((source) => ({ ...source, enabled: false }))),
    onSuccess: () => {
      toast.success(`已禁用 ${cleanupTargets.length} 个失效源 (可随时启用恢复)`);
      setCleanupOpen(false);
      void queryClient.invalidateQueries({ queryKey: SOURCES_QUERY_KEY });
      void queryClient.invalidateQueries({ queryKey: SOURCE_STATS_QUERY_KEY });
    },
    onError: (error) => toast.error(sourceErrorMessage(error, "清理失败")),
  });

  const [keyword, setKeyword] = React.useState("");
  const [activeGroup, setActiveGroup] = React.useState<string>(SOURCE_GROUP_ALL);
  const [importOpen, setImportOpen] = React.useState(false);
  const [editSource, setEditSource] = React.useState<BookSource | null>(null);
  const [loginSource, setLoginSource] = React.useState<BookSource | null>(null);
  /** 书源登录态(cookie 按用户存库): 行徽标 + 登录对话框横幅 */
  const cookiesQuery = useQuery({
    queryKey: SOURCE_COOKIES_QUERY_KEY,
    queryFn: getBookSourceCookie,
    staleTime: 30_000,
  });
  const cookieMap = React.useMemo(() => {
    const m = new Map<string, string>();
    for (const row of cookiesQuery.data ?? []) {
      if (row.cookie.trim().length > 0) m.set(row.sourceUrl, row.cookie);
    }
    return m;
  }, [cookiesQuery.data]);
  const [debugSource, setDebugSource] = React.useState<BookSource | null>(null);
  const [pendingDelete, setPendingDelete] = React.useState<BookSource | null>(null);
  const [invalidOpen, setInvalidOpen] = React.useState(false);

  const groups = React.useMemo(() => buildSourceGroups(sources), [sources]);

  // 当前分组可能因为书源被删空而消失, 回落到「全部」
  const currentGroup = React.useMemo(
    () => (groups.some((item) => item.value === activeGroup) ? activeGroup : SOURCE_GROUP_ALL),
    [groups, activeGroup],
  );


  const invalidUrls = React.useMemo(
    () => new Set(invalid.invalidSources.map((item) => item.sourceUrl)),
    [invalid.invalidSources],
  );
  const [statusFilter, setStatusFilter] = React.useState<"all" | "invalid" | "premium" | "poor">("all");
  /** 失效口径 = 近期失效标记 ∪ 长期统计死源 (与筛选/清理按钮一致) */
  const expiredCount = React.useMemo(
    () =>
      sources.filter(
        (source) =>
          invalidUrls.has(source.bookSourceUrl) || deadUrls.has(source.bookSourceUrl),
      ).length,
    [sources, invalidUrls, deadUrls],
  );
  const [sortByConf, setSortByConf] = React.useState(false);
  const [selectMode, setSelectMode] = React.useState(false);
  const [selected, setSelected] = React.useState<ReadonlySet<string>>(new Set());
  const [invalidInfo, setInvalidInfo] = React.useState<InvalidBookSource | null>(null);
  const invalidMap = React.useMemo(() => {
    const map = new Map<string, InvalidBookSource>();
    for (const item of invalid.invalidSources) {
      map.set(item.sourceUrl, item);
    }
    return map;
  }, [invalid.invalidSources]);
  /** 差源: 置信度<0.4 且尝试≥3 (含垫底源), 供筛选+批量禁用 */
  const poorSet = React.useMemo(() => {
    const set = new Set<string>();
    for (const [url, stat] of statMap) {
      if (stat.confidence < 0.4 && stat.attempts >= 3) {
        set.add(url);
      }
    }
    return set;
  }, [statMap]);
  const premiumSet = React.useMemo(() => {
    const set = new Set<string>();
    for (const [url, stat] of statMap) {
      if (stat.confidence >= 0.7 && stat.attempts >= 5) {
        set.add(url);
      }
    }
    return set;
  }, [statMap]);

  const visibleSources = React.useMemo(() => {
    const base = filterSources(sources, currentGroup, keyword);
    let list = base;
    if (statusFilter === "invalid") {
      list = base.filter((source) => invalidUrls.has(source.bookSourceUrl) || deadUrls.has(source.bookSourceUrl));
    } else if (statusFilter === "premium") {
      list = base.filter((source) => premiumSet.has(source.bookSourceUrl));
    } else if (statusFilter === "poor") {
      list = base.filter((source) => poorSet.has(source.bookSourceUrl));
    }
    if (sortByConf) {
      list = [...list].sort((a, b) => {
        const ca = statMap.get(a.bookSourceUrl)?.confidence ?? -1;
        const cb = statMap.get(b.bookSourceUrl)?.confidence ?? -1;
        return cb - ca;
      });
    }
    return list;
  }, [sources, currentGroup, keyword, statusFilter, invalidUrls, premiumSet, poorSet, sortByConf, statMap]);

  // 只有正在切换的那一行开关进入忙碌态
  const busyUrl = toggleEnabled.isPending ? toggleEnabled.variables?.url : undefined;

  const checkInvalid = () => {
    setInvalidOpen(true);
    invalid.check();
  };

  const clearFilters = () => {
    setKeyword("");
    setActiveGroup(SOURCE_GROUP_ALL);
  };

  const toggleSelected = (source: BookSource): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(source.bookSourceUrl)) {
        next.delete(source.bookSourceUrl);
      } else {
        next.add(source.bookSourceUrl);
      }
      return next;
    });
  };
  const selectedSources = React.useMemo(
    () => sources.filter((source) => selected.has(source.bookSourceUrl)),
    [sources, selected],
  );
  const bulkSetEnabled = useMutation({
    mutationFn: (enabled: boolean) =>
      saveBookSources(selectedSources.map((source) => ({ ...source, enabled }))),
    onSuccess: (_data, enabled) => {
      toast.success(enabled ? `已启用 ${selectedSources.length} 个源` : `已禁用 ${selectedSources.length} 个源`);
      void queryClient.invalidateQueries({ queryKey: SOURCES_QUERY_KEY });
    },
    onError: (error) => toast.error(sourceErrorMessage(error, "批量操作失败")),
  });
  const bulkDelete = useMutation({
    mutationFn: () => deleteSources.mutateAsync(selectedSources.map((s) => s.bookSourceUrl)),
    onSuccess: () => setSelected(new Set()),
  });
  /** 导出选中源为 legado 兼容 JSON, 便于分享他人 */
  const exportSelected = (): void => {
    const blob = new Blob([JSON.stringify(selectedSources, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `reader-sources-${selectedSources.length}.json`;
    a.click();
    URL.revokeObjectURL(url);
    toast.success(`已导出 ${selectedSources.length} 个书源`);
  };

  const confirmDelete = () => {
    const target = pendingDelete;
    setPendingDelete(null);
    if (target === null) return;
    deleteSources.mutate([target.bookSourceUrl]);
  };

  const filterActive = keyword.trim().length > 0 || currentGroup !== SOURCE_GROUP_ALL;
  const enabledCount = sources.filter((source) => source.enabled).length;

  return (
    <div className="mx-auto flex min-h-full w-full max-w-5xl flex-col px-4 pb-10 pt-5 sm:px-6 md:px-10 md:pt-8">
      <PageIntro
        eyebrow="SOURCE CENTER"
        title="书源管理"
        desc="管理搜索来源, 启用后参与检索与更新."
        action={
          <>
            <IconButton
              variant="ghost"
              aria-label="刷新书源列表"
              tooltip="刷新"
              disabled={isLoading || isFetching}
              onClick={refetch}
            >
              <RotateCw aria-hidden className={cn(isFetching && "ui-spin")} />
            </IconButton>
            <Button
              size="sm"
              variant="secondary"
              className="hidden sm:inline-flex"
              onClick={() => navigate("/workbench")}
            >
              <ToolCase aria-hidden />
              工作台
            </Button>
            <Button
              size="sm"
              variant="secondary"
              className="hidden sm:inline-flex"
              onClick={checkInvalid}
            >
              <ShieldAlert aria-hidden />
              检测失效
            </Button>
            <Button
              size="sm"
              variant="secondary"
              className="hidden sm:inline-flex"
              disabled={cleanupTargets.length === 0}
              onClick={() => setCleanupOpen(true)}
            >
              <Trash2 aria-hidden />
              清理失效源{cleanupTargets.length > 0 ? ` ${cleanupTargets.length}` : ""}
            </Button>
            <Button
              size="sm"
              className="hidden sm:inline-flex"
              onClick={() => setImportOpen(true)}
            >
              <Download aria-hidden />
              导入
            </Button>
            {/* 移动端: 导入(带文字主按钮) + 更多菜单(语义完整), 避免纯图标不可读 */}
            <div className="flex items-center gap-2 sm:hidden">
              <Button size="sm" onClick={() => setImportOpen(true)}>
                <Download aria-hidden />
                导入
              </Button>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <IconButton variant="secondary" aria-label="更多操作">
                    <Ellipsis aria-hidden />
                  </IconButton>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuItem onSelect={() => navigate("/workbench")}>
                    <ToolCase aria-hidden />
                    工作台
                  </DropdownMenuItem>
                  <DropdownMenuItem onSelect={checkInvalid}>
                    <ShieldAlert aria-hidden />
                    检测失效
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    disabled={cleanupTargets.length === 0}
                    onSelect={() => setCleanupOpen(true)}
                  >
                    <Trash2 aria-hidden />
                    清理失效源{cleanupTargets.length > 0 ? ` ${cleanupTargets.length}` : ""}
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </>
        }
      />
      <div className="mb-4 flex flex-wrap items-center gap-x-3 gap-y-2">
        <Input
          aria-label="在书源中筛选"
          placeholder="搜索名称 / 地址 / 分组"
          className="min-w-44 flex-1"
          value={keyword}
          onChange={(event) => setKeyword(event.target.value)}
          clearable
          onClear={() => setKeyword("")}
          prefixIcon={<Search aria-hidden className="size-4 text-muted-foreground" />}
        />
        {!isLoading && error === null && sources.length > 0 ? (
          <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
            {sources.length} 个书源 · {enabledCount} 个启用
          </span>
        ) : null}
        <div className="flex shrink-0 flex-wrap items-center gap-1">
          {(["all", "invalid", "premium", "poor"] as const).map((key) => (
            <button
              key={key}
              type="button"
              aria-pressed={statusFilter === key}
              onClick={() => setStatusFilter(key)}
              className={cn(
                "rounded-full border px-2.5 py-1 text-xs transition-colors duration-150 ease-out",
                statusFilter === key
                  ? "border-accent bg-accent/10 text-accent"
                  : "border-border text-muted-foreground hover:text-foreground",
              )}
            >
              {key === "all"
                ? "全部"
                : key === "invalid"
                  ? `失效 ${expiredCount}`
                  : key === "premium"
                    ? `精品 ${premiumSet.size}`
                    : `差 ${poorSet.size}`}
            </button>
          ))}
          <button
            type="button"
            aria-pressed={sortByConf}
            onClick={() => setSortByConf((value) => !value)}
            className={cn(
              "ml-1 rounded-full border px-2.5 py-1 text-xs transition-colors duration-150 ease-out",
              sortByConf
                ? "border-accent bg-accent/10 text-accent"
                : "border-border text-muted-foreground hover:text-foreground",
            )}
          >
            按置信度排序
          </button>
          <button
            type="button"
            aria-pressed={selectMode}
            onClick={() => {
              setSelectMode((value) => !value);
              setSelected(new Set());
            }}
            className={cn(
              "ml-1 rounded-full border px-2.5 py-1 text-xs transition-colors duration-150 ease-out",
              selectMode
                ? "border-accent bg-accent/10 text-accent"
                : "border-border text-muted-foreground hover:text-foreground",
            )}
          >
            多选
          </button>
        </div>
      </div>

      {isLoading ? (
        <SkeletonList count={8} className="py-2" />
      ) : error !== null ? (
        <EmptyState
          icon={<CircleAlert aria-hidden />}
          title="书源列表加载失败"
          description={sourceErrorMessage(error, "网络异常或登录态已失效")}
          action={
            <Button size="sm" variant="secondary" onClick={refetch}>
              <RotateCw aria-hidden />
              重试
            </Button>
          }
          className="rounded-xl border border-dashed border-border py-12"
        />
      ) : sources.length === 0 ? (
        <EmptyState
          icon={<Database aria-hidden />}
          title="还没有书源"
          description="导入 legado 书源 JSON, 或从远程链接拉取书源文件"
          action={
            <Button size="sm" onClick={() => setImportOpen(true)}>
              <Download aria-hidden />
              导入书源
            </Button>
          }
          className="rounded-xl border border-dashed border-border py-12"
        />
      ) : (
        <div className="flex min-h-0 flex-1 flex-col gap-4">
          <SourceGroupTabs
            groups={groups}
            value={currentGroup}
            onChange={setActiveGroup}
            controlsId={LIST_ID}
          />

          {visibleSources.length === 0 ? (
            <EmptyState
              compact
              icon={<Search aria-hidden />}
              title="没有符合条件的书源"
              description={filterActive ? "换个关键词, 或切回「全部」分组看看" : undefined}
              action={
                <Button size="sm" variant="secondary" onClick={clearFilters}>
                  清除筛选
                </Button>
              }
              className="rounded-xl border border-dashed border-border py-12"
            />
          ) : (
            <>
            {selectMode ? (
              <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-surface px-3 py-2">
                <span className="whitespace-nowrap text-sm leading-8 text-muted-foreground">
                  已选
                  <span className="mx-1 font-medium text-foreground tabular-nums">{selected.size}</span>
                  个源
                </span>
                <label className="flex shrink-0 cursor-pointer items-center gap-1.5 text-xs text-muted-foreground">
                  <input
                    type="checkbox"
                    className="size-4 accent-[var(--accent)]"
                    checked={selected.size === visibleSources.length && visibleSources.length > 0}
                    onChange={(event) =>
                      setSelected(event.target.checked ? new Set(visibleSources.map((s) => s.bookSourceUrl)) : new Set())
                    }
                  />
                  全选本页
                </label>
                <div className="ml-auto flex flex-wrap items-center gap-2">
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={selected.size === 0}
                    loading={bulkSetEnabled.isPending && bulkSetEnabled.variables === true}
                    onClick={() => bulkSetEnabled.mutate(true)}
                  >
                    启用
                  </Button>
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={selected.size === 0}
                    loading={bulkSetEnabled.isPending && bulkSetEnabled.variables === false}
                    onClick={() => bulkSetEnabled.mutate(false)}
                  >
                    禁用
                  </Button>
                  <Button size="sm" variant="secondary" disabled={selected.size === 0} onClick={exportSelected}>
                    <Upload aria-hidden />
                    导出
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={selected.size === 0}
                    loading={bulkDelete.isPending}
                    onClick={() => bulkDelete.mutate()}
                  >
                    <Trash2 aria-hidden />
                    删除
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => {
                      setSelectMode(false);
                      setSelected(new Set());
                    }}
                  >
                    取消
                  </Button>
                </div>
              </div>
            ) : null}
            <ul
              id={LIST_ID}
              className="divide-y divide-border/70 overflow-hidden rounded-xl border border-border bg-surface"
            >
              {visibleSources.map((source) => (
                <SourceListItem
                  key={source.bookSourceUrl}
                  source={source}
                  invalid={invalidUrls.has(source.bookSourceUrl)}
                  dead={deadUrls.has(source.bookSourceUrl)}
                  stat={statMap.get(source.bookSourceUrl)}
                  selectMode={selectMode}
                  selected={selected.has(source.bookSourceUrl)}
                  onToggleSelected={toggleSelected}
                  onInvalidClick={(target) => setInvalidInfo(invalidMap.get(target.bookSourceUrl) ?? null)}
                  busy={busyUrl === source.bookSourceUrl}
                  onToggleEnabled={(target, enabled) =>
                    toggleEnabled.mutate({ url: target.bookSourceUrl, enabled })
                  }
                  onWorkbench={(target) =>
                    navigate(`/workbench?url=${encodeURIComponent(target.bookSourceUrl)}`)
                  }
                  logged={cookieMap.has(source.bookSourceUrl)}
                  onEdit={setEditSource}
                  onLogin={setLoginSource}
                  onDebug={setDebugSource}
                  onDelete={setPendingDelete}
                />
              ))}
            </ul>
            </>
          )}
        </div>
      )}

      <ImportSourcesDialog open={importOpen} onOpenChange={setImportOpen} />

      <EditSourceDialog
        source={editSource}
        open={editSource !== null}
        onOpenChange={(open) => {
          if (!open) setEditSource(null);
        }}
      />

      <SourceLoginDialog
        source={loginSource}
        open={loginSource !== null}
        existingCookie={
          loginSource ? (cookieMap.get(loginSource.bookSourceUrl) ?? null) : null
        }
        onCookieChanged={() =>
          void queryClient.invalidateQueries({ queryKey: SOURCE_COOKIES_QUERY_KEY })
        }
        onOpenChange={(open) => {
          if (!open) setLoginSource(null);
        }}
      />

      <DebugSourceDrawer
        source={debugSource}
        open={debugSource !== null}
        onOpenChange={(open) => {
          if (!open) setDebugSource(null);
        }}
      />

      <InvalidSourcesDialog
        open={invalidOpen}
        onOpenChange={setInvalidOpen}
        invalidSources={invalid.invalidSources}
        checking={invalid.isChecking}
        lastCheckedAt={invalid.lastCheckedAt}
        error={invalid.error}
        sources={sources}
        onRetry={invalid.check}
      />

      <Dialog open={cleanupOpen} onOpenChange={setCleanupOpen}>
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>清理失效源</DialogTitle>
            <DialogDescription>
              以下 {cleanupTargets.length} 个源搜索尝试≥5 次且成功率&lt;20% (置信度已垫底),
              禁用后不再参与搜索排序与轮次; 随时可在列表重新启用.
            </DialogDescription>
          </DialogHeader>
          <ul className="max-h-56 overflow-y-auto text-xs text-muted-foreground">
            {cleanupTargets.map((source) => (
              <li key={source.bookSourceUrl} className="truncate py-1">
                {source.bookSourceName || source.bookSourceUrl} · {source.bookSourceUrl}
              </li>
            ))}
          </ul>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setCleanupOpen(false)}>
              取消
            </Button>
            <Button variant="danger" loading={cleanup.isPending} onClick={() => cleanup.mutate()}>
              禁用 {cleanupTargets.length} 个源
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>删除书源</DialogTitle>
            <DialogDescription>
              确定删除「
              {pendingDelete !== null && pendingDelete.bookSourceName.length > 0
                ? pendingDelete.bookSourceName
                : (pendingDelete?.bookSourceUrl ?? "")}
              」吗? 依赖该书源的书籍将无法搜索和更新.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button variant="danger" loading={deleteSources.isPending} onClick={confirmDelete}>
              <Trash2 aria-hidden />
              删除
            </Button>
          </DialogFooter>
        </DialogContent>

      </Dialog>

      <InvalidReasonDialog
        invalid={invalidInfo}
        sources={sources}
        open={invalidInfo !== null}
        onOpenChange={(open) => {
          if (!open) setInvalidInfo(null);
        }}
      />
    </div>
  );
}
