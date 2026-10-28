import type { Book } from "@/types/api";

/** 计算"有效总章数"所需的书字段: 书架记录 + 已读位置 */
type ChapterCountSource = Pick<Book, "totalChapterNum" | "durChapterIndex">;

/**
 * 有效总章数: 书架记录的 `totalChapterNum` 不能当权威用.
 *
 * 夜间书架更新曾把残缺目录的章数回写进 `total_chapter_num`(绍宋: 记录 60 章 /
 * 实际已缓存 400 章), 于是"进度 / 未读数 / 设为读完"全部按错值算. 后端已加质量门
 * 不再回写残缺血, 前端这里的口径是兜底: 取所有已知下界的最大值——
 *
 * - 书架记录 `totalChapterNum`;
 * - 目录实际章数 `tocCount`(阅读器里能拿到 `chapters.length`, 传进来它才权威);
 * - 已读位置 `durChapterIndex + 1`(读到第 N 章 ⇒ 全书至少 N 章).
 *
 * 三者全为 0(书架与目录都没数据)时返回 0, 保持"总章数未知"的退化行为,
 * 由调用方决定怎么显示——不要伪造出 1 章.
 */
export function effectiveTotalChapterNum(book: ChapterCountSource, tocCount = 0): number {
  const shelf = Math.max(0, Math.trunc(book.totalChapterNum) || 0);
  const fromToc = Math.max(0, Math.trunc(tocCount) || 0);
  const known = Math.max(shelf, fromToc);
  if (known === 0) {
    return 0;
  }
  const readUpTo = Math.max(0, Math.trunc(book.durChapterIndex) || 0) + 1;
  return Math.max(known, readUpTo);
}
