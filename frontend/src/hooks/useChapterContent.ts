import { useQuery, useQueryClient } from "@tanstack/react-query";
import DOMPurify from "dompurify";
import { useEffect, useMemo } from "react";

import { proxiedAssetUrl } from "@/lib/asset";
import { getBookContent } from "@/services/book";
import { activeReplaceRules, applyReplaceRules, useReplaceRules } from "@/services/purify";
import type { BookChapter } from "@/types/api";

const ALLOWED_TAGS = [
  "p", "br", "img", "div", "span",
  "h1", "h2", "h3", "h4",
  "em", "strong", "b", "i", "u", "s", "hr",
];
const ALLOWED_ATTR = ["src", "alt", "class", "style", "data-pos"];
const FORBID_TAGS = ["script", "iframe", "style", "link"];

export type ParagraphType = "title" | "paragraph" | "image" | "volumeTitle" | "volumeTag";

/** 渲染用段落条目 (与旧 Content.vue vItems 语义一致) */
export interface ReaderParagraph {
  /** 章节内唯一 key (即渲染顺序) */
  key: number;
  type: ParagraphType;
  /** paragraph/image/volumeTag 为清洗后的 HTML 片段; title/volumeTitle 为纯文本 */
  text: string;
  /**
   * 首行字符索引: 段落首字符在「标题 + "\n\n" + 各段落以 "\n\n" 相连」的整章文本中的位置,
   * 与后端 Book.durChapterPos 语义一致; volumeTag 为 -1 (不参与进度定位).
   */
  pos: number;
}

/**
 * 正文图片地址归一化 (warp 没有 /reader3/cover 代理, 图片直接热链原站):
 * - data:/blob: 原样保留
 * - http(s):// 原样返回; 协议相对地址补 https:
 * - 相对地址 → 以书源域名为基准解析; 无书源域名或解析失败时原样保留
 */
export function resolveImageSrc(src: string, origin?: string): string {
  return proxiedAssetUrl(resolveAbsoluteImageSrc(src, origin), origin);
}

/** 相对地址 → 以书源域名为基准绝对化 (代理重写前的纯路径解析) */
function resolveAbsoluteImageSrc(src: string, origin?: string): string {
  if (/^(?:data:|blob:)/i.test(src)) {
    return src;
  }
  if (/^https?:\/\//i.test(src)) {
    return src;
  }
  if (src.startsWith("//")) {
    return `https:${src}`;
  }
  if (origin !== undefined && origin !== "") {
    try {
      return new URL(src, origin).href;
    } catch {
      return src;
    }
  }
  return src;
}

/** DOMPurify 清洗 (去 script/iframe/style/link 等) + img src 重写 + 懒加载 */
export function sanitizeChapterHtml(html: string, origin?: string): string {
  const fragment = DOMPurify.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    FORBID_TAGS,
    RETURN_DOM_FRAGMENT: true,
  });
  fragment.querySelectorAll("img").forEach((img) => {
    const src = img.getAttribute("src");
    if (src !== null && src !== "") {
      img.setAttribute("src", resolveImageSrc(src, origin));
    }
    img.setAttribute("loading", "lazy");
    img.setAttribute("decoding", "async");
  });
  const holder = document.createElement("div");
  holder.appendChild(fragment);
  return holder.innerHTML;
}

/**
 * 按 \n+ 切分段落数组 (语义与旧 Content.vue vItems 完全一致):
 * - 普通章节: 首项为章节标题 (pos=0); 之后每个非空段落 pos 依次为
 *   title.length + 2 (标题后两个换行符), 每段再累加 segment.length + 2,
 *   即该段首行字符在整章文本中的索引, 与 durChapterPos 可直接比较;
 * - 分卷章节 (isVolume): 卷标题 (pos=0) + 卷说明 (pos=-1), 居中渲染, 不切正文段落.
 */
export function buildParagraphs(
  title: string,
  sanitizedContent: string,
  isVolume: boolean,
): ReaderParagraph[] {
  const items: ReaderParagraph[] = [];
  if (isVolume) {
    items.push({ key: items.length, type: "volumeTitle", text: title, pos: 0 });
    const tag = sanitizedContent.trim();
    if (tag !== "") {
      items.push({ key: items.length, type: "volumeTag", text: tag, pos: -1 });
    }
    return items;
  }
  items.push({ key: items.length, type: "title", text: title, pos: 0 });
  let wordCount = title.length + 2; // 2 为标题后的两个换行符
  for (const rawSegment of sanitizedContent.split(/\n+/)) {
    const segment = rawSegment.replace(/^\s+/g, "");
    if (segment === "") {
      continue;
    }
    items.push({
      key: items.length,
      type: segment.includes("<img") ? "image" : "paragraph",
      text: segment,
      pos: wordCount,
    });
    wordCount += segment.length + 2; // 2 为段落间的分隔换行符
  }
  return items;
}

export interface UseChapterContentResult {
  /** 净化+清洗+切分后的段落数组; 未加载完成时为空 */
  items: ReaderParagraph[];
  /** 章节标题 (来自章节列表, 已按 scopeTitle 规则净化) */
  title: string;
  isPending: boolean;
  isError: boolean;
  errorMessage: string;
  /** 请求成功但正文 (净化后) 为空 */
  isEmpty: boolean;
  refetch: () => void;
}

/**
 * 当前章节正文: 拉取 → 净化规则替换 → DOMPurify 清洗 → 图片热链重写 → 按 \n+ 切段 (含 pos).
 * 章节列表就绪 (chapter 存在) 后才发起请求: getBookContent 会让后端顺手写阅读进度,
 * 书籍信息未就绪时抢跑 index=0 会污染书架进度.
 * 当前章节加载成功后自动预取下一章 (cache=1, 后端不会为预取章节写阅读进度).
 * origin 双重用途: warp getBookContent 的 bookSource 选源提示 + 相对图片地址的解析基准.
 */
export function useChapterContent(
  bookUrl: string,
  index: number,
  chapter: BookChapter | undefined,
  origin?: string,
  chapterCount?: number,
  nextChapterUrl?: string,
  nextChapterUrl2?: string,
  nextChapterTitle?: string,
  nextChapterTitle2?: string,
): UseChapterContentResult {
  const queryClient = useQueryClient();

  // warp getBookContent 的 url 参数是章节页 URL (Kotlin 旧形为 bookUrl+index 内部解析);
  // 章节列表就绪后才发起, 避免 index=0 抢跑污染书架进度.
  const contentQuery = useQuery({
    queryKey: ["content", bookUrl, index],
    queryFn: () =>
      getBookContent(chapter?.url ?? bookUrl, index, {
        bookSourceUrl: origin,
        bookUrl,
        title: chapter?.title,
      }),
    enabled: bookUrl !== "" && index >= 0 && chapter !== undefined,
    // 服务端章节缓存命中即秒回; staleTime 让章间往返/回退重进走客户端缓存
    staleTime: 5 * 60_000,
  });

  const isSuccess = contentQuery.isSuccess;
  // 预加载后续两章: 缓存未覆盖到的阅读前沿时, 翻页前大概率已缓存秒开
  useEffect(() => {
    if (!isSuccess || bookUrl === "") {
      return;
    }
    for (const [offset, url, nextTitle] of [
      [1, nextChapterUrl, nextChapterTitle],
      [2, nextChapterUrl2, nextChapterTitle2],
    ] as const) {
      if (url === undefined) {
        continue;
      }
      const nextIndex = index + offset;
      if (chapterCount !== undefined && nextIndex >= chapterCount) {
        continue;
      }
      void queryClient.prefetchQuery({
        queryKey: ["content", bookUrl, nextIndex],
        queryFn: () =>
          getBookContent(url, nextIndex, {
            cache: true,
            bookSourceUrl: origin,
            bookUrl,
            title: nextTitle,
          }),
        staleTime: 5 * 60_000,
      });
    }
  }, [queryClient, bookUrl, index, isSuccess, chapterCount, nextChapterUrl, nextChapterUrl2, origin]);

  const rawTitle = chapter?.title ?? "";
  const isVolume = chapter?.isVolume ?? false;
  const rawContent = contentQuery.data?.content ?? "";

  // 净化规则: warp 只做规则 CRUD, 不把规则应用到正文, 所以替换发生在渲染期 ——
  // 清洗前对原始正文/章标题按 order 升序跑一遍; 规则改动经 ["replaceRules"] 失效后,
  // 已缓存的原始正文立即按新规则重渲染, 不需要重新抓章节.
  const { rules } = useReplaceRules();
  const contentRules = useMemo(() => activeReplaceRules(rules, "content"), [rules]);
  const titleRules = useMemo(() => activeReplaceRules(rules, "title"), [rules]);
  const title = useMemo(() => applyReplaceRules(rawTitle, titleRules), [rawTitle, titleRules]);
  const content = useMemo(
    () => applyReplaceRules(rawContent, contentRules),
    [rawContent, contentRules],
  );

  const items = useMemo(
    () =>
      isSuccess ? buildParagraphs(title, sanitizeChapterHtml(content, origin), isVolume) : [],
    [isSuccess, title, content, origin, isVolume],
  );

  const error = contentQuery.error;

  return {
    items,
    title,
    isPending: contentQuery.isLoading,
    isError: contentQuery.isError,
    errorMessage: error instanceof Error && error.message !== "" ? error.message : "正文加载失败",
    isEmpty: isSuccess && content.trim() === "",
    refetch: () => {
      void contentQuery.refetch();
    },
  };
}
