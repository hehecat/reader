import { Search as SearchIcon } from "lucide-react";
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
} from "@/components/ui";
import { humanizeError } from "@/lib/errors";
import { searchBook } from "@/services/search";
import type { BookSource, SearchBook } from "@/types/api";

export interface SourceProbeDialogProps {
  source: BookSource | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 预填关键词(通常取最近一次搜索词) */
  initialKey?: string;
}

/**
 * 单源直搜: 只打这一个源的搜索接口, 秒级出结果 —
 * 验证"这个源现在到底能不能用", 不用等全窗 SSE 跑完.
 */
export function SourceProbeDialog({
  source,
  open,
  onOpenChange,
  initialKey = "",
}: SourceProbeDialogProps) {
  const [key, setKey] = React.useState(initialKey);
  const [busy, setBusy] = React.useState(false);
  const [books, setBooks] = React.useState<SearchBook[] | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [elapsed, setElapsed] = React.useState<number | null>(null);

  React.useEffect(() => {
    if (open) {
      setKey(initialKey);
      setBooks(null);
      setError(null);
      setElapsed(null);
    }
  }, [open, initialKey]);

  if (!source) return null;

  const run = async () => {
    const trimmed = key.trim();
    if (trimmed.length === 0) {
      setError("请输入关键词");
      return;
    }
    setBusy(true);
    setError(null);
    setBooks(null);
    const t0 = performance.now();
    try {
      const hits = await searchBook(trimmed, source.bookSourceUrl, 1);
      setElapsed(performance.now() - t0);
      setBooks(hits);
    } catch (e) {
      setElapsed(performance.now() - t0);
      setError(humanizeError(e instanceof Error ? e.message : String(e)));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <SearchIcon aria-hidden className="size-4" />
            单源直搜 · {source.bookSourceName}
          </DialogTitle>
          <DialogDescription className="truncate">
            {source.bookSourceUrl} · 只搜这一个源, 用于快速验证可用性
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3 px-4 pb-4 md:px-5 md:pb-5">
          <div className="flex items-center gap-2">
            <Input
              value={key}
              autoFocus
              placeholder="关键词, 如 大主宰"
              onChange={(e) => setKey(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void run();
              }}
            />
            <Button disabled={busy} onClick={() => void run()}>
              {busy ? <Spinner size="sm" /> : null}
              搜索
            </Button>
          </div>

          {elapsed !== null ? (
            <p className="text-muted-foreground text-xs">
              {books !== null ? `耗时 ${Math.round(elapsed)}ms · ${books.length} 条结果` : `失败 · 耗时 ${Math.round(elapsed)}ms`}
            </p>
          ) : null}

          {error !== null ? (
            <p className="text-destructive text-sm whitespace-pre-line">{error}</p>
          ) : null}

          {books !== null && books.length === 0 ? (
            <p className="text-muted-foreground text-sm">该源返回 0 条 — 搜索规则可能已失效或该站无此书</p>
          ) : null}

          {books !== null && books.length > 0 ? (
            <ul className="border-border/70 max-h-72 divide-y divide-border/70 overflow-y-auto rounded-md border">
              {books.map((b) => (
                <li key={`${b.bookUrl}|${b.origin}`} className="px-3 py-2">
                  <p className="truncate text-sm font-medium">{b.name}</p>
                  <p className="text-muted-foreground truncate text-xs">
                    {b.author || "未知作者"}
                    {b.latestChapterTitle ? ` · ${b.latestChapterTitle}` : ""}
                  </p>
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}
