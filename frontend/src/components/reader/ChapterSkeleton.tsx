import { cn, Skeleton } from "@/components/ui";

/**
 * 章节未命中缓存时的骨架屏: 按正文排版画「章头 + 若干段落」的形状 ——
 * 居中转圈只能告诉读者在等, 段落形状能告诉他等的是什么, 且命中后替换时版面不跳.
 * 尺寸全部走 --reader-* 变量, 与真实正文同宽同字号.
 */

/** 段落内文本行的宽度序列: 末行故意短一点, 读起来才像正文 */
const LINE_WIDTHS = ["99%", "96%", "98%", "93%"];

export interface ChapterSkeletonProps {
  /** 摆几段; 默认 7 段, 大约一屏半 */
  paragraphs?: number;
  className?: string;
}

export function ChapterSkeleton({ paragraphs = 7, className }: ChapterSkeletonProps) {
  return (
    <div
      className={cn("h-full overflow-hidden", className)}
      role="status"
      aria-busy="true"
      aria-live="polite"
    >
      <span className="sr-only">正在加载章节…</span>
      <div
        className="mx-auto px-5 pt-10 pb-8"
        style={{ maxWidth: "var(--reader-content-width, 42em)" }}
      >
        {/* 章头: 书名·作者小字 + 章节标题 */}
        <Skeleton shape="text" className="h-3 w-28" />
        <Skeleton shape="text" className="mt-3 h-6 w-3/5" />
        <div
          aria-hidden
          className="mt-10 flex flex-col"
          style={{ gap: "var(--reader-paragraph-gap)" }}
        >
          {Array.from({ length: paragraphs }, (_, index) => {
            // 3~5 行一段, 长短交错, 免得整块灰条纹看着像表格
            const lines = 3 + ((index * 2) % 3);
            return (
              <div key={index} className="flex flex-col" style={{ gap: "0.5em" }}>
                {Array.from({ length: lines }, (_, line) => (
                  <Skeleton
                    key={line}
                    shape="text"
                    className="rounded-sm"
                    style={{
                      height: "calc(var(--reader-line-height) * 1em)",
                      width:
                        line === lines - 1 ? "50%" : LINE_WIDTHS[line % LINE_WIDTHS.length],
                    }}
                  />
                ))}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
