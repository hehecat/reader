/**
 * 关系图谱的生成入口弹窗 (确认 → 进度 → 完成).
 *
 * 三道闸都在这里:
 *   1. 预估卡片 (GET /graphEstimate): 多少章、分几批、大概花多少钱 —— 用户点「开始生成」才算确认;
 *   2. 进度条 (GET /graphSSE): 完成批数/总批数 + 状态文案, 中途可取消;
 *   3. 完成: 「查看图谱」跳 /graph?url=<bookUrl>.
 *
 * 阅读页和 /graph 空态共用它, 所以数据与状态都在弹窗内部管理, 调用方只管开着还是关着.
 */
import { useQuery } from "@tanstack/react-query";
import { CircleAlert, Network, RotateCw } from "lucide-react";
import * as React from "react";
import { useNavigate } from "react-router-dom";

import { Button, Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, Spinner, cn } from "@/components/ui";
import {
  getGraphEstimate,
  isGraphTerminal,
  useGraphGeneration,
  type GraphEstimate,
  type GraphProgress,
} from "@/services/graph";

export interface GraphGenerateDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  bookUrl: string;
  /** 书名(拿不到时留空, 标题退化成「生成关系图谱」) */
  bookName?: string;
  /** 点「查看图谱」: 不传则跳 /graph?url=<bookUrl> (已经在该页时由页面自己刷新) */
  onView?: () => void;
}

/** 进度状态的人话 (queued/running 都是"正在跑", 只是位置不同) */
function statusText(progress: GraphProgress): string {
  switch (progress.status) {
    case "queued":
      return "排队中…";
    case "running":
      return "正在逐批抽取人物与关系…";
    case "done":
      return "生成完成";
    case "failed":
      return "生成失败";
    case "cancelled":
      return "已取消";
    default:
      return "处理中…";
  }
}

/** token 数按万折叠: 12_345 → "1.2 万" */
function formatTokens(tokens: number): string {
  if (tokens >= 10_000) {
    return `${(tokens / 10_000).toFixed(1)} 万`;
  }
  return tokens.toLocaleString("zh-CN");
}

export function GraphGenerateDialog({
  open,
  onOpenChange,
  bookUrl,
  bookName = "",
  onView,
}: GraphGenerateDialogProps) {
  const navigate = useNavigate();
  const generation = useGraphGeneration(bookUrl);
  const progress = generation.progress;
  const running = progress !== null && !isGraphTerminal(progress.status);

  // 每次打开都重新预估: 章数/缓存在变化, 也不用缓存旧价格
  const estimateQuery = useQuery({
    queryKey: ["graphEstimate", bookUrl],
    queryFn: () => getGraphEstimate(bookUrl),
    enabled: open && bookUrl !== "",
    staleTime: 0,
    gcTime: 0,
    retry: 0,
  });

  const handleOpenChange = React.useCallback(
    (next: boolean) => {
      if (!next) {
        generation.reset();
      }
      onOpenChange(next);
    },
    [generation, onOpenChange],
  );

  const handleStart = React.useCallback(() => {
    void generation.start(bookName === "" ? {} : { bookName });
  }, [bookName, generation]);

  // 强制重跑：清空台账回到「首次生成」。只埋在这一处的次要按钮里，
  // 避免用户误点把已抽好的 1745 章再烧一遍。
  const handleForceStart = React.useCallback(() => {
    void generation.start(
      bookName === "" ? { force: true } : { bookName, force: true },
    );
  }, [bookName, generation]);

  const handleView = React.useCallback(() => {
    handleOpenChange(false);
    if (onView !== undefined) {
      onView();
      return;
    }
    navigate(`/graph?url=${encodeURIComponent(bookUrl)}`);
  }, [bookUrl, handleOpenChange, navigate, onView]);

  const estimate = estimateQuery.data;
  const percent =
    progress !== null && progress.totalBatches > 0
      ? Math.min(100, Math.round((progress.doneBatches / progress.totalBatches) * 100))
      : 0;

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent width="md">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Network className="size-4 shrink-0 text-accent" aria-hidden />
            <span className="truncate">
              {bookName === "" ? "生成关系图谱" : `生成《${bookName}》的关系图谱`}
            </span>
          </DialogTitle>
          <DialogDescription>
            {progress === null
              ? "会把已缓存的章节分批交给 AI，抽取人物、势力与关系。生成过程可随时取消。"
              : running
                ? "任务在后台跑，关掉这个窗口不会中断；回来重新打开即可看到进度。"
                : "任务已结束。"}
          </DialogDescription>
        </DialogHeader>

        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-4 pb-1 md:px-5">
          {progress === null ? (
            <EstimateBlock
              loading={estimateQuery.isLoading}
              error={
                estimateQuery.isError
                  ? (estimateQuery.error as Error).message || "预估失败"
                  : null
              }
              estimate={estimate}
              onRetry={() => void estimateQuery.refetch()}
            />
          ) : (
            <ProgressBlock progress={progress} percent={percent} />
          )}

          {generation.error !== null ? (
            <p className="flex items-start gap-1.5 rounded-lg border border-danger/40 bg-danger/10 px-2.5 py-2 text-xs leading-relaxed text-danger">
              <CircleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
              <span className="break-words">{generation.error}</span>
            </p>
          ) : null}
        </div>

        <DialogFooter>
          {progress === null ? (
            <>
              <Button variant="secondary" onClick={() => handleOpenChange(false)}>
                取消
              </Button>
              {estimate?.upToDate === true ? (
                <Button variant="secondary" loading={generation.starting} onClick={handleForceStart}>
                  强制重跑全部
                </Button>
              ) : null}
              <Button
                variant="primary"
                loading={generation.starting}
                disabled={estimate === undefined || estimate.batches === 0}
                onClick={handleStart}
              >
                开始生成
              </Button>
            </>
          ) : running ? (
            <>
              <Button variant="secondary" onClick={() => handleOpenChange(false)}>
                后台继续
              </Button>
              <Button variant="danger" onClick={() => void generation.cancel()}>
                取消生成
              </Button>
            </>
          ) : progress.status === "done" ? (
            <>
              <Button variant="secondary" onClick={() => handleOpenChange(false)}>
                关闭
              </Button>
              <Button variant="primary" onClick={handleView}>
                查看图谱
              </Button>
            </>
          ) : (
            <>
              <Button variant="secondary" onClick={() => handleOpenChange(false)}>
                关闭
              </Button>
              <Button variant="primary" onClick={handleStart} loading={generation.starting}>
                <RotateCw aria-hidden />
                重新生成
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

interface EstimateBlockProps {
  loading: boolean;
  error: string | null;
  estimate: GraphEstimate | undefined;
  onRetry: () => void;
}

/** 预估卡片: 章数 / 批数 / token + 花费说明 —— 这就是「确认框」的全部内容 */
function EstimateBlock({ loading, error, estimate, onRetry }: EstimateBlockProps) {
  if (loading) {
    return (
      <div className="flex items-center gap-2 rounded-xl border border-border bg-surface-muted/50 px-3 py-3 text-xs text-muted-foreground">
        <Spinner size="sm" />
        正在统计已缓存章节…
      </div>
    );
  }
  if (error !== null) {
    return (
      <div className="space-y-2 rounded-xl border border-danger/40 bg-danger/10 px-3 py-2.5 text-xs text-danger">
        <p className="flex items-start gap-1.5 leading-relaxed">
          <CircleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden />
          <span className="break-words">预估失败：{error}</span>
        </p>
        <Button size="sm" variant="secondary" onClick={onRetry}>
          重试
        </Button>
      </div>
    );
  }
  if (estimate === undefined) {
    return null;
  }
  return (
    <div className="space-y-2">
      <dl className="space-y-1.5 rounded-xl border border-border bg-surface-muted/50 px-3 py-2.5 text-xs">
        <Row label="已缓存章节" value={`${estimate.cachedChapters} 章`} />
        <Row
          label="本次覆盖"
          value={
            estimate.upToDate
              ? "已是最新，无需重跑"
              : `${estimate.selectedChapters} 章（增量，不重跑已抽过的）`
          }
        />
        <Row label="分批数" value={`${estimate.batches} 批`} />
        <Row label="预估输入 token" value={formatTokens(estimate.estPromptTokens)} />
      </dl>
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        {estimate.estCostNote !== ""
          ? estimate.estCostNote
          : "生成会消耗 AI 模型额度，具体花费取决于所用模型的价格。"}
      </p>
      {estimate.batches === 0 ? (
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          {estimate.upToDate
            ? "全部章节都已抽取过且正文没变化：已是最新，不用再花一遍 token。真要重抽，可用右下角「强制重跑全部」（花费同首次生成）。"
            : "还没有可用的缓存章节：先去「缓存全本」把正文抓下来，再来生成图谱。"}
        </p>
      ) : null}
    </div>
  );
}

/** 进度块: 状态文案 + doneBatches/totalBatches + 已抽出的人物与关系数 */
function ProgressBlock({ progress, percent }: { progress: GraphProgress; percent: number }) {
  const failed = progress.status === "failed";
  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="font-medium text-foreground">{statusText(progress)}</span>
        <span className="shrink-0 tabular-nums text-muted-foreground">
          {progress.doneBatches} / {progress.totalBatches} 批
        </span>
      </div>
      <div
        role="progressbar"
        aria-label="生成进度"
        aria-valuemin={0}
        aria-valuemax={progress.totalBatches || 0}
        aria-valuenow={progress.totalBatches > 0 ? progress.doneBatches : undefined}
        className="h-2 w-full overflow-hidden rounded-full bg-surface-muted"
      >
        <div
          className={cn(
            "h-full rounded-full transition-[width] duration-300 ease-out",
            failed ? "bg-danger" : "bg-accent",
            progress.totalBatches === 0 && "w-1/3 animate-pulse",
          )}
          style={progress.totalBatches > 0 ? { width: `${percent}%` } : undefined}
        />
      </div>
      <p className="text-[11px] leading-relaxed text-muted-foreground">
        已抽出 {progress.characters} 个人物 · {progress.relations} 条关系
      </p>
      {progress.error !== null ? (
        <p className="break-words rounded-lg border border-danger/40 bg-danger/10 px-2.5 py-2 text-[11px] leading-relaxed text-danger">
          {progress.error}
        </p>
      ) : null}
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <dt className="shrink-0 text-muted-foreground">{label}</dt>
      <dd className="truncate text-right tabular-nums text-foreground">{value}</dd>
    </div>
  );
}
