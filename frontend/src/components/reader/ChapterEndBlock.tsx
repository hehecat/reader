import { ChevronLeft, ChevronRight } from "lucide-react";
import * as React from "react";
import { useSearchParams } from "react-router-dom";

import { Button, toast } from "@/components/ui";
import { startCacheBookStream } from "@/services/cache";
import { useSettingsStore } from "@/stores/settings-store";

/** 章末预热的会话级去重: 同一 bookUrl 本次会话只静默启动一次缓存 (刷新页面后重置) */
const preheatedBooks = new Set<string>();

export interface ChapterEndBlockProps {
  /** 当前章节序号 (0 起) */
  index: number;
  /** 章节总数 */
  total: number;
  /** 下一章标题; null 表示已是最后一章 */
  nextTitle: string | null;
  onPrevChapter: () => void;
  onNextChapter: () => void;
}

/**
 * 章末标记: 纯落点, 不带按钮 —— 细分隔饰线 + 「本章完 · 下一章标题」, 末章收在「全书完」.
 * 少了它, 滚到底只剩一大片空白, 读者不知道章已经结束; 跳章交给底栏与下方导航, 不再重复放「下一章」.
 */
function ChapterEndMarker({ nextTitle }: { nextTitle: string | null }) {
  return (
    <div className="break-inside-avoid pt-14 text-center">
      <div aria-hidden className="mx-auto mb-7 flex max-w-40 items-center gap-3 text-border">
        <span className="h-px flex-1 bg-current" />
        <span className="size-1 shrink-0 rotate-45 bg-current" />
        <span className="h-px flex-1 bg-current" />
      </div>
      {nextTitle === null ? (
        <p className="font-display text-sm tracking-[0.3em] text-muted-foreground">全书完</p>
      ) : (
        <p className="text-sm leading-7 text-muted-foreground">
          本章完
          <span aria-hidden> · </span>
          <span className="font-display text-foreground/85">{nextTitle}</span>
        </p>
      )}
    </div>
  );
}

/**
 * 章末块 (滚动模式的正文末尾 / 翻页模式的最后一页): 章末标记 → 上一章/下一章导航.
 * 章末滚入视口且设置里开着「章末自动预热」时静默启动一次整书后台缓存 (cacheBookSSE, 按书去重),
 * 进度可在目录抽屉附着查看; 开关只在设置页, 章末不再重复放一个开关.
 * 「重新拉取本章」入口在目录抽屉底部, 章末只留最朴素的分隔与翻页.
 */
export function ChapterEndBlock({
  index,
  total,
  nextTitle,
  onPrevChapter,
  onNextChapter,
}: ChapterEndBlockProps) {
  // 书链接取自阅读页路由参数 (章末块由 Virtuoso context 透传, 不走 props 链)
  const [searchParams] = useSearchParams();
  const bookUrl = searchParams.get("url") ?? "";
  const isLocal = bookUrl === "" || bookUrl.startsWith("local://");

  const footerRef = React.useRef<HTMLElement | null>(null);
  const closeStreamRef = React.useRef<(() => void) | null>(null);

  const tryPreheat = React.useCallback(() => {
    if (isLocal || preheatedBooks.has(bookUrl)) {
      return;
    }
    preheatedBooks.add(bookUrl);
    toast.info("后台缓存已启动");
    // 静默启动: 本地流只为陪跑到终态, 失败不打扰阅读 (去重保留, 避免逐章重试);
    // 服务端任务独立运行, 目录抽屉打开时按同 url 附着即可看到进度
    closeStreamRef.current?.();
    closeStreamRef.current = startCacheBookStream(bookUrl, {
      onProgress: () => {},
      onDone: () => {
        closeStreamRef.current = null;
      },
      onError: (err) => {
        closeStreamRef.current = null;
        console.warn("[reader] 章末预热失败:", err.message);
      },
    });
  }, [bookUrl, isLocal]);

  // 章末块滚入视口 (翻页模式翻到末页) 且开关开启时触发一次预热
  React.useEffect(() => {
    const node = footerRef.current;
    if (node === null) {
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          if (useSettingsStore.getState().preheatOnChapterEnd) {
            tryPreheat();
          }
        }
      },
      { threshold: 0.1 },
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, [tryPreheat]);

  // 卸载 (切章/离开阅读页) 只断本地进度流, 服务端缓存任务继续
  React.useEffect(() => () => closeStreamRef.current?.(), []);

  return (
    <footer ref={footerRef} className="break-inside-avoid px-5 pt-10 pb-16">
      <ChapterEndMarker nextTitle={nextTitle} />

      <nav className="mt-12 flex items-center justify-between gap-3 border-t border-border pt-6 text-sm">
        <Button
          variant="ghost"
          size="sm"
          disabled={index <= 0}
          onClick={onPrevChapter}
          aria-label="上一章"
        >
          <ChevronLeft aria-hidden />
          上一章
        </Button>
        <span className="text-xs text-muted-foreground tabular-nums">
          {index + 1} / {total}
        </span>
        <Button
          variant="ghost"
          size="sm"
          disabled={index >= total - 1}
          onClick={onNextChapter}
          aria-label="下一章"
        >
          下一章
          <ChevronRight aria-hidden />
        </Button>
      </nav>

    </footer>
  );
}
