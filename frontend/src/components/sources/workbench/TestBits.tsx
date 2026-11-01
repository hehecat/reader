import { CircleAlert, Info, TriangleAlert } from "lucide-react";
import * as React from "react";

import { sourceErrorMessage } from "@/components/sources/useSources";
import { CoverImage, cn } from "@/components/ui";
import { getCoverUrl } from "@/services/book";
import type { BookSource, SearchBook } from "@/types/api";

/** 每个测试 tab 的公共入参: 编辑器现场组装的源(null=校验未过) + 不可用原因 */
export interface TestTabProps {
  source: BookSource | null;
  /** 源不可用原因(校验摘要); 非空时运行按钮禁用并展示该提示 */
  blockedReason: string | null;
}

/** 每个测试 tab 的运行态: running/error/耗时/结果, 结果在重跑期间保留旧值 */
export interface TestRunState<T> {
  running: boolean;
  error: string | null;
  ms: number | null;
  result: T | null;
}

const IDLE: TestRunState<never> = { running: false, error: null, ms: null, result: null };

export interface TestRun<T> extends TestRunState<T> {
  run: (task: () => Promise<T>, errorFallback: string) => Promise<void>;
  reset: () => void;
}

/** 测试运行封装: 计时 + 错误归一化成 humanizeError 人话(原始串进 console) */
export function useTestRun<T>(): TestRun<T> {
  const [state, setState] = React.useState<TestRunState<T>>(IDLE);

  const run = React.useCallback(async (task: () => Promise<T>, errorFallback: string) => {
    setState((prev) => ({ ...prev, running: true, error: null }));
    const started = performance.now();
    try {
      const result = await task();
      setState({ running: false, error: null, ms: Math.round(performance.now() - started), result });
    } catch (error) {
      setState((prev) => ({
        ...prev,
        running: false,
        error: sourceErrorMessage(error, errorFallback),
        ms: Math.round(performance.now() - started),
      }));
    }
  }, []);

  const reset = React.useCallback(() => setState(IDLE as TestRunState<T>), []);
  return { ...state, run, reset };
}

/** 运行失败: danger 提示行(role=alert 供读屏播报) */
export function TestError({ message }: { message: string }) {
  return (
    <p
      role="alert"
      className="flex items-start gap-2 rounded-lg border border-danger/40 bg-danger/5 px-3 py-2 text-sm leading-6 text-danger"
    >
      <CircleAlert aria-hidden className="mt-1 size-4 shrink-0" />
      <span className="min-w-0 break-all">{message}</span>
    </p>
  );
}

/** 前置条件提示(源缺 searchUrl 等): 中性警示, 不是运行错误 */
export function TestHint({ message }: { message: string }) {
  return (
    <p className="flex items-start gap-2 rounded-lg border border-border bg-surface-muted px-3 py-2 text-sm leading-6 text-muted-foreground">
      <TriangleAlert aria-hidden className="mt-1 size-4 shrink-0" />
      <span className="min-w-0">{message}</span>
    </p>
  );
}

/** 结果元信息行: 条数/字数/耗时等 */
export function TestMeta({ children }: { children: React.ReactNode }) {
  return <p className="text-xs text-muted-foreground tabular-nums">{children}</p>;
}

/** 结果区外框: 统一间距, 空结果时给中性提示 */
export function TestResult({
  empty,
  emptyText,
  children,
}: {
  /** 结果集为空(运行成功但 0 条) */
  empty?: boolean;
  emptyText?: string;
  children: React.ReactNode;
}) {
  if (empty === true) {
    return (
      <p className="flex items-center gap-2 rounded-lg border border-dashed border-border px-3 py-4 text-sm text-muted-foreground">
        <Info aria-hidden className="size-4 shrink-0" />
        {emptyText ?? "解析结果为空 — 规则可能没命中页面结构, 可切换整源 JSON 对照 legado 示例检查"}
      </p>
    );
  }
  return <div className="flex flex-col gap-2">{children}</div>;
}

/** 搜索/书海结果列表: 封面缩略 + 名称/作者/简介 */
export function BookHitList({ books }: { books: SearchBook[] }) {
  return (
    <ul className="divide-y divide-border/70 overflow-hidden rounded-lg border border-border">
      {books.map((book, index) => (
        <li key={`${book.bookUrl}-${index}`} className="flex gap-3 px-3 py-2.5">
          <CoverImage
            src={getCoverUrl(book.coverUrl)}
            alt={book.name}
            author={book.author}
            className="w-10 shrink-0"
          />
          <div className="min-w-0 flex-1">
            <p className="flex items-baseline gap-2 text-sm">
              <span className="min-w-0 truncate font-medium" title={book.name}>
                {book.name}
              </span>
              {book.author.length > 0 ? (
                <span className="shrink-0 text-xs text-muted-foreground">{book.author}</span>
              ) : null}
            </p>
            {book.intro !== undefined && book.intro.length > 0 ? (
              <p className="mt-0.5 line-clamp-2 text-xs leading-5 text-muted-foreground">
                {book.intro}
              </p>
            ) : null}
            <p className="mt-0.5 truncate text-xs text-muted-foreground" title={book.bookUrl}>
              {book.bookUrl}
            </p>
          </div>
        </li>
      ))}
    </ul>
  );
}

/** 输入 + 运行按钮的一行(移动端换行堆叠) */
export function RunRow({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return <div className={cn("flex flex-wrap items-center gap-2", className)}>{children}</div>;
}
