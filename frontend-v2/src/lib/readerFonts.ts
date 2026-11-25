import type { ReaderFont } from "@/stores/prefs";

export type BuiltinReaderFont = Exclude<ReaderFont, "custom">;

export interface ReaderFontPreset {
  id: BuiltinReaderFont;
  label: string;
  tag: string;
  description: string;
  cssVar: string;
  /** 可选开源按需 WebFont CSS（仅选中时异步加载，离线自动优雅回退至本机字体栈） */
  webfontUrls?: readonly string[];
}

const NOTO_SERIF_URL =
  "https://fonts.loli.net/css2?family=Noto+Serif+SC:wght@400;600&display=swap";
const NOTO_SANS_URL =
  "https://fonts.loli.net/css2?family=Noto+Sans+SC:wght@400;500;600&display=swap";
const LXGW_WENKAI_SCREEN_URL =
  "https://cdn.jsdelivr.net/npm/lxgw-wenkai-screen-webfont@1.7.0/style.css";

export const READER_FONT_PRESETS: readonly ReaderFontPreset[] = [
  {
    id: "serif",
    label: "思源宋体",
    tag: "经典书韵",
    description: "思源宋体 / 方正博雅宋 / 华文宋体，横细竖粗、长篇耐读",
    cssVar: "var(--font-serif)",
    webfontUrls: [NOTO_SERIF_URL],
  },
  {
    id: "kai",
    label: "霞鹜文楷",
    tag: "温润清雅",
    description: "霞鹜文楷屏幕阅读版，兼顾楷体温度与电子屏清晰度",
    cssVar: "var(--font-kai)",
    webfontUrls: [LXGW_WENKAI_SCREEN_URL],
  },
  {
    id: "oldsong",
    label: "刻本老宋",
    tag: "古籍出版",
    description: "京華老宋体 / 方正清刻本悦宋，历史武侠古典仙侠首选",
    cssVar: "var(--font-oldsong)",
    webfontUrls: [NOTO_SERIF_URL],
  },
  {
    id: "fangsong",
    label: "兰亭仿宋",
    tag: "清隽秀挺",
    description: "方正兰亭宋 / 方正悠宋 / 仿宋，笔画清朗、行文舒展",
    cssVar: "var(--font-fangsong)",
    webfontUrls: [NOTO_SERIF_URL],
  },
  {
    id: "sans",
    label: "现代黑体",
    tag: "利落清晰",
    description: "苹方 / 冬青黑体 / 微软雅黑 / 思源黑体，屏幕辨识度极高",
    cssVar: "var(--font-sans)",
    webfontUrls: [NOTO_SANS_URL],
  },
  {
    id: "round",
    label: "柔润楷圆",
    tag: "轻松柔和",
    description: "方正准圆 / 方正楷体 / 圆体，比黑体柔和、无细笔抖动",
    cssVar: "var(--font-round)",
    webfontUrls: [LXGW_WENKAI_SCREEN_URL],
  },
] as const;

const PRESET_BY_ID = new Map<BuiltinReaderFont, ReaderFontPreset>(
  READER_FONT_PRESETS.map((p) => [p.id, p]),
);

const injectedStylesheets = new Set<string>();

/** 按需异步注入开源阅读字体 CSS（幂等、非阻塞、离线安全回退） */
export function ensureReaderFontLoaded(fontFamily: ReaderFont): void {
  if (typeof document === "undefined" || fontFamily === "custom") return;
  const preset = PRESET_BY_ID.get(fontFamily);
  if (!preset?.webfontUrls?.length) return;

  for (const href of preset.webfontUrls) {
    if (injectedStylesheets.has(href)) continue;
    injectedStylesheets.add(href);
    if (document.querySelector(`link[rel="stylesheet"][href="${href}"]`)) continue;
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = href;
    link.crossOrigin = "anonymous";
    document.head.appendChild(link);
  }
}

export function resolveReaderFontFamily(
  fontFamily: ReaderFont,
  customFontFamily: string,
): string {
  if (fontFamily === "custom" && customFontFamily) {
    return `"${customFontFamily}", var(--font-serif)`;
  }
  const preset = PRESET_BY_ID.get(fontFamily as BuiltinReaderFont);
  return preset ? preset.cssVar : "var(--font-serif)";
}
