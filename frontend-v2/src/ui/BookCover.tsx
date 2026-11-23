import * as React from "react";
import { resolveImageUrl } from "@/api/client";
import { cleanIntro } from "@/lib/format";
import { cn } from "@/lib/cn";
import { motion, SPRING_SNAPPY, useReducedMotion } from "./motion";

/** 砚台 6 组古籍书衣配色：朱砂 · 靛青 · 石青 · 赭石 · 松绿 · 绛紫 */
const SPINE_GRADIENTS: ReadonlyArray<readonly [from: string, to: string]> = [
  ["#8a3d34", "#4a211d"],
  ["#3c4a7d", "#232a4c"],
  ["#2f6d75", "#1c4148"],
  ["#6b5a2f", "#3a3018"],
  ["#4a6b3c", "#243a1e"],
  ["#6d3f63", "#3a2135"],
];

function hashTitle(title: string): number {
  let hash = 2166136261;
  for (let i = 0; i < title.length; i += 1) {
    hash ^= title.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return Math.abs(hash);
}

function spineBackground(title: string): string {
  const [from, to] = SPINE_GRADIENTS[hashTitle(title) % SPINE_GRADIENTS.length] ?? SPINE_GRADIENTS[0]!;
  // 斜向布纹 + 主色渐变 + 右上柔光，模拟织锦书衣
  return [
    "radial-gradient(120% 80% at 85% 0%, rgba(255,255,255,0.14), transparent 55%)",
    "repeating-linear-gradient(45deg, rgba(255,255,255,0.025) 0 2px, transparent 2px 4px)",
    `linear-gradient(150deg, ${from}, ${to})`,
  ].join(", ");
}

/** 书脊压痕：暗槽 + 高光脊线 + 柔和过渡，在真实封面与替身封面上通用 */
const SPINE_GROOVE =
  "linear-gradient(90deg, rgba(0,0,0,0.30) 0%, rgba(0,0,0,0.12) 3.2%, rgba(255,255,255,0.20) 4.6%, rgba(255,255,255,0.04) 6.2%, rgba(0,0,0,0.10) 7.6%, transparent 12%)";

/** 封面顶部漫反射 + 底部压暗，赋予纸板厚度感 */
const COVER_SHEEN =
  "linear-gradient(180deg, rgba(255,255,255,0.10) 0%, transparent 22%, transparent 78%, rgba(0,0,0,0.10) 100%)";

export interface BookCoverProps {
  src?: string | null;
  referer?: string;
  title: string;
  author?: string | null;
  intro?: string | null;
  interactive?: boolean;
  pageEdges?: boolean;
  className?: string;
}

export function BookCover({
  src,
  referer,
  title,
  author,
  intro,
  interactive = true,
  pageEdges = false,
  className,
}: BookCoverProps) {
  const resolvedSrc = React.useMemo(() => resolveImageUrl(src, referer), [src, referer]);
  const [failedUrl, setFailedUrl] = React.useState<string | null>(null);
  const [loadedUrl, setLoadedUrl] = React.useState<string | null>(null);
  const [hovering, setHovering] = React.useState(false);
  const imgRef = React.useRef<HTMLImageElement>(null);
  const reduced = useReducedMotion();

  // 浏览器内存缓存命中时，确保同步完成的封面立即可见
  React.useEffect(() => {
    const img = imgRef.current;
    if (!resolvedSrc || !img || !img.complete) return;
    if (img.naturalWidth >= 8 && img.naturalHeight >= 8) {
      setLoadedUrl(resolvedSrc);
    } else if (img.naturalWidth === 0) {
      setFailedUrl(resolvedSrc);
    }
  }, [resolvedSrc]);

  const showFallback = !resolvedSrc || failedUrl === resolvedSrc;
  const isLoaded = Boolean(resolvedSrc && loadedUrl === resolvedSrc);
  const isLoading = Boolean(resolvedSrc) && !showFallback && !isLoaded;
  const authorText = author?.trim() ?? "";
  const introText = React.useMemo(() => cleanIntro(intro), [intro]);
  const lifted = interactive && !reduced && hovering;

  return (
    <div
      onPointerEnter={(e) => {
        if (e.pointerType === "mouse" && interactive) setHovering(true);
      }}
      onPointerLeave={() => setHovering(false)}
      className={cn("relative select-none", className)}
    >
      {/* 书页厚度：封面右下方叠出几层纸边 */}
      {pageEdges ? (
        <span
          aria-hidden
          className="pointer-events-none absolute inset-y-[2.5%] -right-[3px] left-[6%] rounded-r-[5px] bg-[#f3eee3] shadow-[inset_-1px_0_0_rgba(0,0,0,0.08)] dark:bg-[#cfc7b8]"
          style={{
            backgroundImage:
              "repeating-linear-gradient(90deg, transparent 0 1px, rgba(0,0,0,0.07) 1px 1.5px)",
            backgroundPosition: "right",
            backgroundSize: "3px 100%",
            backgroundRepeat: "repeat-y",
          }}
        />
      ) : null}

      <motion.div
        animate={lifted ? { y: -3, rotate: -0.4 } : { y: 0, rotate: 0 }}
        transition={SPRING_SNAPPY}
        className={cn(
          "pointer-events-none relative aspect-3/4 w-full overflow-hidden rounded-l-[3px] rounded-r-md ring-1 ring-black/10 transition-shadow duration-300 dark:ring-white/10",
          lifted ? "shadow-[var(--shadow-book-hover)]" : "shadow-[var(--shadow-book)]",
        )}
      >
        {/* 底层替身封面：无图、加载中或解码失败时始终托底，避免空白块 */}
        {!isLoaded || showFallback ? (
          <div
            style={{ backgroundImage: spineBackground(title || "无题"), color: "oklch(0.97 0.02 90)" }}
            className="@container pointer-events-none relative flex size-full flex-col justify-between p-[11%] pl-[17%]"
          >
            {/* 线装订线：沿书脊一列针眼与缝线（窄封面自动隐藏） */}
            <span
              aria-hidden
              className="pointer-events-none absolute inset-y-[6%] left-[6.5%] hidden w-[3px] @[72px]:block"
              style={{
                backgroundImage:
                  "radial-gradient(circle, rgba(0,0,0,0.38) 0 1px, transparent 1.4px), linear-gradient(rgba(255,255,255,0.32), rgba(255,255,255,0.32))",
                backgroundSize: "3px 18%, 1px 100%",
                backgroundRepeat: "repeat-y, no-repeat",
                backgroundPosition: "center top, center",
              }}
            />

            {/* 题签：米白纸签 + 双线边框，书名以墨色宋体书写 */}
            <div className="relative min-w-0 self-stretch rounded-[2px] bg-[#f5efe2]/95 px-[8%] py-[7%] text-[#2b2620] shadow-[0_1px_2px_rgba(0,0,0,0.25)] ring-1 ring-black/10 ring-inset @max-[56px]:bg-transparent @max-[56px]:p-0 @max-[56px]:text-current @max-[56px]:shadow-none @max-[56px]:ring-0">
              <span
                aria-hidden
                className="pointer-events-none absolute inset-[3px] hidden rounded-[1px] border border-[#8c3b28]/25 @[96px]:block"
              />
              <p className="relative line-clamp-3 font-serif text-[clamp(9px,12.5cqi,17px)] leading-[1.25] font-semibold text-balance break-all">
                {title || "未命名"}
              </p>
              {authorText ? (
                <p className="relative mt-[0.35em] hidden truncate text-[clamp(9px,8.5cqi,11px)] tracking-wide text-[#2b2620]/60 @[86px]:block">
                  {authorText}
                </p>
              ) : null}
            </div>

            {introText ? (
              <p className="relative hidden line-clamp-3 font-serif text-[10.5px] leading-snug text-white/72 @[124px]:block">
                {introText}
              </p>
            ) : (
              <span
                aria-hidden
                className="relative hidden size-[14cqi] max-h-6 max-w-6 items-center justify-center self-end rounded-[2px] border border-[#e9a294]/55 font-serif text-[clamp(7px,7cqi,12px)] text-[#f1b3a6]/80 @[110px]:flex"
              >
                {(title || "砚").slice(0, 1)}
              </span>
            )}

            {/* 加载骨架流光：真实封面加载中时覆盖于替身封面之上 */}
            {isLoading && !reduced ? <span aria-hidden className="skeleton-sheen" /> : null}
          </div>
        ) : null}

        {!showFallback && resolvedSrc ? (
          <img
            ref={imgRef}
            src={resolvedSrc}
            alt={title}
            decoding="async"
            loading="lazy"
            draggable={false}
            onLoad={(e) => {
              const img = e.currentTarget;
              if (img.naturalWidth < 8 || img.naturalHeight < 8) {
                setFailedUrl(resolvedSrc);
              } else {
                setLoadedUrl(resolvedSrc);
              }
            }}
            onError={() => setFailedUrl(resolvedSrc)}
            className={cn(
              "pointer-events-none absolute inset-0 size-full object-cover transition-[opacity,filter,transform] duration-500 ease-out",
              isLoaded ? "scale-100 opacity-100 blur-0" : "scale-[1.03] opacity-0 blur-[6px]",
            )}
          />
        ) : null}

        {/* 书脊压痕 + 纸板光泽：真实封面与替身封面共用 */}
        <span
          aria-hidden
          className="pointer-events-none absolute inset-0"
          style={{ backgroundImage: `${SPINE_GROOVE}, ${COVER_SHEEN}` }}
        />
      </motion.div>
    </div>
  );
}
