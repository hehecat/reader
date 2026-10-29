import * as React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { Network, RefreshCw, Sparkles } from "lucide-react";

import { GraphGenerateDialog } from "@/components/graph-demo/GraphGenerateDialog";
import { Badge } from "@/components/ui/Badge";
import { Button } from "@/components/ui/Button";
import { Spinner } from "@/components/ui/Spinner";
import { getBookGraph, getGraphEstimate, type BookGraphData } from "@/services/graph";

export interface GraphTabCardProps {
  bookUrl: string;
  bookName: string;
}

/** 度数最高的几个人: 面板里只放一小撮, 全图去整页看 */
function topByDegree(data: BookGraphData, limit = 6): { name: string; degree: number }[] {
  const degree = new Map<string, number>();
  for (const relation of data.relations) {
    degree.set(relation.from, (degree.get(relation.from) ?? 0) + 1);
    degree.set(relation.to, (degree.get(relation.to) ?? 0) + 1);
  }
  const names = new Map(data.characters.map((character) => [character.id, character.name]));
  return [...degree.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, limit)
    .map(([id, count]) => ({ name: names.get(id) ?? id, degree: count }));
}

/** 一万 token 起按「万」折叠: 12_345 → "1.2 万" */
function formatTokens(tokens: number): string {
  return tokens >= 10_000 ? `${(tokens / 10_000).toFixed(1)} 万` : tokens.toLocaleString("zh-CN");
}

/**
 * 「人物关系」标签页: 读整本批量生成的成品 (GET /reader3/graph)。
 *
 * 以前这里是单发 6 个片段让模型现场抽一张小图(人物十几、关系几十, 每换一章就变);
 * 现在整本生成走同一套 normalize 管线, 面板里只放摘要 + 入口, 画布去整页看 ——
 * 侧栏最窄 384px, 塞三视图会把它压成只能看个轮廓。
 */
export function GraphTabCard({ bookUrl, bookName }: GraphTabCardProps) {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [dialogOpen, setDialogOpen] = React.useState(false);

  const graphQuery = useQuery({
    queryKey: ["bookGraph", bookUrl],
    queryFn: () => getBookGraph(bookUrl),
    enabled: bookUrl !== "",
    staleTime: 60 * 1000,
    retry: 0,
  });
  const data = graphQuery.data?.data ?? null;
  const hasData = graphQuery.data?.hasData === true && data !== null;

  // 没有成品时顺手预估一下: 让用户点生成之前就知道要花多少
  const estimateQuery = useQuery({
    queryKey: ["graphEstimate", bookUrl],
    queryFn: () => getGraphEstimate(bookUrl),
    enabled: bookUrl !== "" && !hasData && graphQuery.isSuccess,
    staleTime: 0,
    gcTime: 0,
    retry: 0,
  });
  const estimate = estimateQuery.data ?? null;
  const top = React.useMemo(() => (data === null ? [] : topByDegree(data)), [data]);

  const startGenerate = React.useCallback(() => setDialogOpen(true), []);
  const openFullPage = React.useCallback(
    () => navigate(`/graph?url=${encodeURIComponent(bookUrl)}`),
    [bookUrl, navigate],
  );

  const dialog = (
    <GraphGenerateDialog
      open={dialogOpen}
      onOpenChange={setDialogOpen}
      bookUrl={bookUrl}
      bookName={bookName}
      onView={() => {
        // 生成完留在面板里看摘要; 想看画布点「打开图谱」
        void queryClient.invalidateQueries({ queryKey: ["bookGraph", bookUrl] });
      }}
    />
  );

  if (graphQuery.isLoading) {
    return (
      <div className="flex min-h-0 flex-1 items-center justify-center gap-2 text-sm text-muted-foreground">
        <Spinner size="sm" label="读取图谱" />
        正在读取关系图谱…
      </div>
    );
  }

  if (!hasData || data === null) {
    return (
      <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 py-4">
        <div className="flex items-center gap-2">
          <Network aria-hidden className="size-4 text-accent" />
          <p className="text-sm font-medium">还没有整本关系图谱</p>
        </div>
        <p className="text-xs leading-relaxed text-muted-foreground">
          把已缓存的章节分批送进模型，抽人物、身份与关系，再统一去重、校方向、算代差。
          整本跑一次大约几分钟，之后每次打开都是同一张图。
        </p>
        {estimate === null ? (
          <p className="text-xs text-muted-foreground">
            {estimateQuery.isError ? "读不到预估(后端版本不支持?)" : "正在估算成本…"}
          </p>
        ) : (
          <ul className="space-y-1 rounded-lg border border-border/70 bg-surface-muted/60 px-3 py-2 text-xs text-muted-foreground">
            <li>可送模型的已缓存章节：{estimate.cachedChapters} 章</li>
            <li>分批：{estimate.batches} 批</li>
            <li>预估输入：{formatTokens(estimate.estPromptTokens)} token</li>
            {estimate.estCostNote === "" ? null : <li>花费：{estimate.estCostNote}</li>}
          </ul>
        )}
        <Button onClick={startGenerate} disabled={bookUrl === ""}>
          <Sparkles aria-hidden />
          生成整本图谱
        </Button>
        {graphQuery.isError ? (
          <p className="text-xs text-danger">
            {(graphQuery.error as Error).message || "读取成品失败"}
          </p>
        ) : null}
        {dialog}
      </div>
    );
  }

  const stats = data.stats;
  const characters = stats?.characters ?? data.characters.length;
  const relations = stats?.relations ?? data.relations.length;
  const factions = stats?.factions ?? data.factions.length;
  const generatedAt = data.meta.generatedAt;
  const sampled = data.meta.sampledChapters;
  const total = data.meta.totalChapters;

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-3 py-4">
      <div className="flex items-center gap-2">
        <Network aria-hidden className="size-4 text-accent" />
        <p className="min-w-0 flex-1 truncate text-sm font-medium">
          {data.meta.book === "" ? bookName || "本书" : data.meta.book} · 关系图谱
        </p>
        <Badge size="sm" variant="muted">
          整本
        </Badge>
      </div>

      <div className="grid grid-cols-3 gap-2">
        {[
          { label: "人物", value: characters },
          { label: "关系", value: relations },
          { label: "势力", value: factions },
        ].map((item) => (
          <div
            key={item.label}
            className="rounded-lg border border-border/70 bg-surface-muted/60 px-2 py-2 text-center"
          >
            <p className="text-base font-semibold tabular-nums">{item.value}</p>
            <p className="text-[11px] text-muted-foreground">{item.label}</p>
          </div>
        ))}
      </div>

      <p className="text-xs leading-relaxed text-muted-foreground">
        抽样 {sampled}/{total} 章
        {generatedAt === "" ? "" : ` · 生成于 ${generatedAt}`}
      </p>

      {top.length === 0 ? null : (
        <div className="flex flex-wrap gap-1.5">
          {top.map((item) => (
            <Badge key={item.name} size="sm" variant="muted">
              {item.name} · {item.degree}
            </Badge>
          ))}
        </div>
      )}

      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="primary" onClick={openFullPage}>
          打开图谱
        </Button>
        <Button size="sm" variant="secondary" onClick={startGenerate}>
          <RefreshCw aria-hidden />
          重新生成
        </Button>
      </div>
      <p className="text-xs text-muted-foreground/80">
        家族树 / 势力 / 全景三视图、搜索与导出都在整页里。
      </p>
      {dialog}
    </div>
  );
}
