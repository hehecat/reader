import { useMutation, useQueryClient } from "@tanstack/react-query";

import { toast } from "@/components/ui";
import { saveBook, type SaveBookInput } from "@/services/bookshelf";
import type { Book, SearchBook } from "@/types/api";
import { ensureCacheStream } from "@/services/cache";
import { useSettingsStore } from "@/stores/settings-store";

/** SearchBook → saveBook 入参: 其余 Book 字段由后端实体默认值补齐 */
export function toSaveBookInput(book: SearchBook): SaveBookInput {
  return {
    bookUrl: book.bookUrl,
    tocUrl: book.tocUrl,
    origin: book.origin,
    originName: book.originName,
    name: book.name,
    author: book.author,
    kind: book.kind,
    coverUrl: book.coverUrl,
    intro: book.intro,
    latestChapterTitle: book.latestChapterTitle,
    totalChapterNum: 0,
    // 入架即允许书架刷新更新章数: 缺省 canUpdate 后端实体默认 false(DB 默认 0),
    // 会让"刷新书架"跳过本书(list_updatable_books 只取 can_update=1), 章数永远落后.
    // 本地书(local:// / *.txt)无源可抓, 保持 false.
    canUpdate: !isLocalBookUrl(book.bookUrl),
  };
}

/** 本地书判定: 与后端 run_shelf_update 的跳过条件一致 */
function isLocalBookUrl(bookUrl: string): boolean {
  return bookUrl.startsWith("local://") || bookUrl.endsWith(".txt");
}

export interface UseAddToShelfOptions {
  /** 成功 toast 文案; 传 null 静默(「开始阅读」直接跳转), 缺省 "已加入书架" */
  successToast?: string | null;
  /** 保存成功(invalidate 之后)回调, 「开始阅读」用它导航到阅读器 */
  onSaved?: (saved: Book, source: SearchBook) => void;
}

/** 把搜索结果保存到书架; 成功后失效 ["books"] 缓存让书架页刷新 */
export function useAddToShelf(options: UseAddToShelfOptions = {}) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (book: SearchBook) => saveBook(toSaveBookInput(book)),
    onSuccess: (saved, source) => {
      void queryClient.invalidateQueries({ queryKey: ["books"] });
      const message = options.successToast ?? "已加入书架";
      if (message !== null) {
        toast.success(message);
      }
      // 加架即后台预热整书: 首开几十秒的目录+正文抓取前移到此刻 (设置可关)
      if (useSettingsStore.getState().preheatOnAdd) {
        ensureCacheStream(saved.bookUrl);
      }
      options.onSaved?.(saved, source);
    },
    onError: (error) => {
      toast.error(error instanceof Error && error.message.length > 0 ? error.message : "加入书架失败");
    },
  });
}
