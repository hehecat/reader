import { CircleAlert, Plus, RotateCw, Rss as RssIcon, Search } from "lucide-react";
import * as React from "react";

import { ArticleReaderDialog } from "@/components/rss/ArticleReaderDialog";
import { EditRssSourceDialog } from "@/components/rss/EditRssSourceDialog";
import { RssArticleList } from "@/components/rss/RssArticleList";
import { RssSourceListItem } from "@/components/rss/RssSourceListItem";
import {
  rssErrorMessage,
  useDeleteRssSource,
  useRssSources,
  useToggleRssEnabled,
} from "@/components/rss/useRss";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  Input,
  PageIntro,
  SkeletonList,
  cn,
} from "@/components/ui";
import type { RssArticle, RssSource } from "@/services/rss";

/** 关键词过滤: 不区分大小写, 匹配名称/地址/分组/备注 */
function filterRssSources(sources: RssSource[], keyword: string): RssSource[] {
  const needle = keyword.trim().toLowerCase();
  if (needle.length === 0) return sources;
  return sources.filter((source) =>
    [source.sourceName, source.sourceUrl, source.sourceGroup, source.sourceComment].some((field) =>
      field.toLowerCase().includes(needle),
    ),
  );
}

/**
 * RSS 订阅页: 左(移动端: 上层)订阅列表 + 右(移动端: 下层)选中订阅的文章列表,
 * 文章阅读走全屏/居中对话框. 启停开关乐观更新, 写后本地写回缓存规避后端 5s 读缓存.
 */
export default function RssPage() {
  const { sources, isLoading, isFetching, error, refetch } = useRssSources();
  const toggleEnabled = useToggleRssEnabled();
  const deleteSource = useDeleteRssSource();

  const [keyword, setKeyword] = React.useState("");
  const [selectedUrl, setSelectedUrl] = React.useState<string | null>(null);
  const [editOpen, setEditOpen] = React.useState(false);
  const [editSource, setEditSource] = React.useState<RssSource | null>(null);
  const [pendingDelete, setPendingDelete] = React.useState<RssSource | null>(null);
  const [viewArticle, setViewArticle] = React.useState<RssArticle | null>(null);
  const [readLinks, setReadLinks] = React.useState<ReadonlySet<string>>(() => new Set<string>());

  const selected = sources.find((source) => source.sourceUrl === selectedUrl) ?? null;

  // 选中源被删除/过滤后不存在时回落(加载中不清, 避免首帧闪跳)
  React.useEffect(() => {
    if (!isLoading && selectedUrl !== null && selected === null) {
      setSelectedUrl(null);
    }
  }, [isLoading, selectedUrl, selected]);

  const visibleSources = React.useMemo(
    () => filterRssSources(sources, keyword),
    [sources, keyword],
  );

  // 只有正在切换的那一行开关进入忙碌态
  const busyUrl = toggleEnabled.isPending ? toggleEnabled.variables?.url : undefined;

  const openArticle = (article: RssArticle) => {
    setViewArticle(article);
    if (article.link.length > 0) {
      setReadLinks((prev) => {
        if (prev.has(article.link)) return prev;
        const next = new Set(prev);
        next.add(article.link);
        return next;
      });
    }
  };

  const confirmDelete = () => {
    if (pendingDelete === null) return;
    deleteSource.mutate(pendingDelete.sourceUrl);
    setPendingDelete(null);
  };

  const deleteName =
    pendingDelete !== null && pendingDelete.sourceName.length > 0
      ? pendingDelete.sourceName
      : (pendingDelete?.sourceUrl ?? "");

  const enabledCount = sources.filter((source) => source.enabled).length;

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col px-4 pb-10 pt-5 sm:px-6 md:px-10 md:pt-8">
      <PageIntro
        eyebrow="RSS READER"
        title="RSS 订阅"
        desc="把更新与书单集中到这里, 保持安静而持续的输入."
        action={
          <>
            {/* 刷新放进页首动作区: 移动端只留图标, 不再单独占一行 */}
            <Button
              size="sm"
              variant="secondary"
              aria-label="刷新订阅"
              disabled={isLoading || isFetching}
              onClick={refetch}
            >
              <RotateCw aria-hidden className={cn(isFetching && "ui-spin")} />
              <span className="hidden sm:inline">刷新订阅</span>
            </Button>
            <Button
              size="sm"
              onClick={() => {
                setEditSource(null);
                setEditOpen(true);
              }}
            >
              <Plus aria-hidden />
              添加订阅
            </Button>
          </>
        }
      />

      {!isLoading && error === null && sources.length > 0 ? (
        <p className="mb-5 hidden text-xs text-muted-foreground tabular-nums sm:block">
          {sources.length} 个订阅 · {enabledCount} 个启用
        </p>
      ) : null}

      <div className="flex min-h-0 flex-col gap-5 md:flex-row md:items-start">
        <section
          aria-label="订阅列表"
          className={cn(
            "w-full min-w-0 shrink-0 md:w-80 lg:w-96",
            selected !== null && "hidden md:block",
          )}
        >
          {/* 过滤只作用于左栏: 过滤框与列表同卡片, 列表行 divide-y 分隔 */}
          <div className="rounded-xl border border-border bg-surface p-4">
            <Input
              aria-label="在订阅中筛选"
              placeholder="搜索名称 / 地址 / 分组"
              className="mb-3 max-w-full"
              value={keyword}
              onChange={(event) => setKeyword(event.target.value)}
              clearable
              onClear={() => setKeyword("")}
              prefixIcon={<Search aria-hidden className="size-4 text-muted-foreground" />}
            />
            {isLoading ? (
              <SkeletonList count={4} className="py-1" />
            ) : error !== null ? (
              <EmptyState
                compact
                icon={<CircleAlert aria-hidden />}
                title="订阅列表加载失败"
                description={rssErrorMessage(error, "网络异常或登录态已失效")}
                action={
                  <Button size="sm" variant="secondary" onClick={refetch}>
                    <RotateCw aria-hidden />
                    重试
                  </Button>
                }
                className="rounded-xl border border-dashed border-border py-10"
              />
            ) : sources.length === 0 ? (
              <EmptyState
                compact
                icon={<RssIcon aria-hidden />}
                title="还没有订阅"
                description="添加 RSS 订阅源, 填写地址与解析规则后即可阅读更新"
                action={
                  <Button
                    size="sm"
                    onClick={() => {
                      setEditSource(null);
                      setEditOpen(true);
                    }}
                  >
                    <Plus aria-hidden />
                    添加订阅
                  </Button>
                }
                className="rounded-xl border border-dashed border-border py-10"
              />
            ) : visibleSources.length === 0 ? (
              <EmptyState
                compact
                icon={<Search aria-hidden />}
                title="没有符合条件的订阅"
                description="换个关键词试试"
                action={
                  <Button size="sm" variant="secondary" onClick={() => setKeyword("")}>
                    清除筛选
                  </Button>
                }
                className="rounded-xl border border-dashed border-border py-10"
              />
            ) : (
              <ul className="divide-y divide-border/70">
                {visibleSources.map((source) => (
                  <RssSourceListItem
                    key={source.sourceUrl}
                    source={source}
                    selected={selectedUrl === source.sourceUrl}
                    busy={busyUrl === source.sourceUrl}
                    onSelect={(target) => setSelectedUrl(target.sourceUrl)}
                    onToggleEnabled={(target, enabled) =>
                      toggleEnabled.mutate({ url: target.sourceUrl, enabled })
                    }
                    onEdit={(target) => {
                      setEditSource(target);
                      setEditOpen(true);
                    }}
                    onDelete={setPendingDelete}
                  />
                ))}
              </ul>
            )}
          </div>
        </section>

        <section
          aria-label="文章列表"
          className={cn("min-h-0 min-w-0 flex-1", selected === null && "hidden md:block")}
        >
          {selected === null ? (
            <EmptyState
              className="rounded-xl border border-dashed border-border py-16 md:py-24"
              icon={<RssIcon aria-hidden />}
              title="选择一个订阅"
              description="在左侧列表中点击订阅, 查看它的文章更新"
            />
          ) : (
            <RssArticleList
              key={selected.sourceUrl}
              source={selected}
              readLinks={readLinks}
              onOpenArticle={openArticle}
              onBack={() => setSelectedUrl(null)}
            />
          )}
        </section>
      </div>

      <EditRssSourceDialog
        source={editSource}
        sources={sources}
        open={editOpen}
        onOpenChange={setEditOpen}
      />

      <ArticleReaderDialog
        source={selected}
        article={viewArticle}
        open={viewArticle !== null}
        onOpenChange={(open) => {
          if (!open) setViewArticle(null);
        }}
      />

      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>删除订阅</DialogTitle>
            <DialogDescription>
              确定删除「{deleteName}」吗? 该订阅的地址与解析规则将一并移除.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button variant="danger" loading={deleteSource.isPending} onClick={confirmDelete}>
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
