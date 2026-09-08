import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { FONTS_QUERY_KEY, fontFormat, fontUrl, getFontList, type CustomFont } from "@/services/fonts";

const STYLE_ID = "reader-custom-fonts";

/**
 * 自定义字体: 拉取当前用户已上传字体列表, 并把 @font-face 规则注入 document
 * (阅读器/设置页预览统一生效); 返回列表与刷新函数供设置页管理.
 */
export function useCustomFonts(): {
  fonts: CustomFont[];
  isLoading: boolean;
  refresh: () => void;
} {
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: FONTS_QUERY_KEY,
    queryFn: () => getFontList(),
    staleTime: 60_000,
  });

  const fonts = query.data ?? [];

  React.useEffect(() => {
    const css = fonts
      .map(
        (f) =>
          `@font-face{font-family:"${f.family}";src:url("${fontUrl(f.id)}") format("${fontFormat(f.name)}");font-display:swap;}`,
      )
      .join("\n");
    let el = document.getElementById(STYLE_ID) as HTMLStyleElement | null;
    if (!el) {
      el = document.createElement("style");
      el.id = STYLE_ID;
      document.head.appendChild(el);
    }
    el.textContent = css;
    return () => {
      // 组件卸载不移除: 全局一次注入, 多消费方共存
    };
  }, [fonts]);

  return {
    fonts,
    isLoading: query.isPending,
    refresh: () => {
      void queryClient.invalidateQueries({ queryKey: FONTS_QUERY_KEY });
    },
  };
}
