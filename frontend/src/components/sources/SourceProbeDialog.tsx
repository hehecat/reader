import { Search as SearchIcon } from "lucide-react";
import * as React from "react";
import { useQueryClient } from "@tanstack/react-query";

import { BookDetailDialog } from "@/components/book/BookDetailDialog";
import { SearchResultCard } from "@/components/search/SearchResultCard";
import { BOOKS_QUERY_KEY, useBookshelf } from "@/hooks/useBookshelf";

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
  /** 点卡片 → 复用搜索页详情弹窗(简介/目录/阅读/换源) */
  const [selected, setSelected] = React.useState<SearchBook | null>(null);
  const queryClient = useQueryClient();
  const { books: shelfBooks } = useBookshelf();
  const shelfKeys = React.useMemo(
    () => new Set(shelfBooks.flatMap((b) => [b.bookUrl, `${b.name}\u0000${b.author ?? ""}`])),
    [shelfBooks],
  );
  const handleAdded = React.useCallback(() => {
    void queryClient.invalidateQueries({ queryKey: BOOKS_QUERY_KEY });
  }, [queryClient]);

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
      // 单源搜索可能命中慢源(后端单源超时上限 60s): 放宽前端超时, 别被 30s 默认值提前判死
      const hits = await searchBook(trimmed, source.bookSourceUrl, 1, { timeout: 65_000 });
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
            <div className="flex max-h-80 flex-col gap-2 overflow-y-auto pr-1">
              {books.map((b) => (
                <SearchResultCard
                  key={`${b.bookUrl}|${b.origin}`}
                  layout="list"
                  book={b}
                  keyword={key}
                  inShelf={
                    shelfKeys.has(b.bookUrl) ||
                    shelfKeys.has(`${b.name}\u0000${b.author ?? ""}`)
                  }
                  onSelect={setSelected}
                  onAdded={handleAdded}
                />
              ))}
            </div>
          ) : null}
        </div>

        {/* 复用搜索页详情弹窗: 简介/封面/目录/开始阅读/加入书架/换源都在这里 */}
        {selected !== null ? (
          <BookDetailDialog
            book={selected}
            open
            onOpenChange={(open) => {
              if (!open) setSelected(null);
            }}
            onAdded={handleAdded}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
