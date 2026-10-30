/**
 * 关系图谱 demo 的视觉编码: 门派配色 + 关系线型 + cytoscape 样式表.
 *
 * 编码约定 (给产品/作者看的重点):
 *   颜色 = 门派/势力 (faction 顺序取调色板)
 *   形状 = 身份 (主角星形 / 势力领袖圆角矩形 / 其余椭圆)
 *   线型 + 箭头 = 关系类型 (见 RELATION_VISUAL)
 *
 * 标签不是默认常开的: 默认 label 为空串, 由 graphLabels.syncLabels 按缩放级别
 * 给节点挂 .gd-label / .gd-label-strong, 避免 fit 视图下 65 个名字糊成一片.
 */
import type cytoscape from "cytoscape";

import type { RelationType } from "@/types/graphDemo";

/** 门派配色: 8 色循环, 明暗主题下都够对比度(canvas 不认 CSS 变量, 只能写死) */
export const FACTION_PALETTE = [
  "#b3453a",
  "#2f6fb5",
  "#2f8a5c",
  "#7a5bb5",
  "#a8752a",
  "#1f7f8c",
  "#b0467e",
  "#5d7a2e",
] as const;

/** 门派没写 faction 时的中性色 */
export const NEUTRAL_COLOR = "#8b8178";

/** 关系类型的视觉编码 */
export interface RelationVisual {
  /** 中文关系名(数据里没有 label 时兜底) */
  name: string;
  color: string;
  lineStyle: "solid" | "dashed" | "dotted";
  width: number;
  /** 是否需要箭头(有向关系: 师承/敌对) */
  arrow: boolean;
  /** 极性: 血亲 / 正向 / 负向 / 中性 —— 全景布局用它强调冲突 */
  polarity: "kin" | "pos" | "neg" | "neutral";
}

export const RELATION_VISUAL: Record<RelationType, RelationVisual> = {
  parent: { name: "亲子", color: "#8f949b", lineStyle: "solid", width: 2, arrow: false, polarity: "kin" },
  // 夫妻: cytoscape 的边只有 solid/dashed/dotted, 没有双线, 用粉紫长虚线区分
  spouse: { name: "夫妻", color: "#cf6d92", lineStyle: "dashed", width: 2.2, arrow: false, polarity: "kin" },
  sibling: { name: "手足", color: "#b3b8be", lineStyle: "solid", width: 1.1, arrow: false, polarity: "kin" },
  master: { name: "师承", color: "#3d7ee0", lineStyle: "solid", width: 2, arrow: true, polarity: "pos" },
  disciple: { name: "弟子", color: "#3aa6d8", lineStyle: "solid", width: 1.8, arrow: true, polarity: "pos" },
  ally: { name: "同盟", color: "#2f9e64", lineStyle: "solid", width: 1.7, arrow: false, polarity: "pos" },
  enemy: { name: "敌对", color: "#dc4b4b", lineStyle: "dashed", width: 2.2, arrow: true, polarity: "neg" },
  other: { name: "其他", color: "#b3aca2", lineStyle: "dotted", width: 1, arrow: false, polarity: "neutral" },
};

/** 关系顺序: 图例与面板里的排列顺序 */
export const RELATION_ORDER: RelationType[] = [
  "parent",
  "spouse",
  "sibling",
  "master",
  "disciple",
  "ally",
  "enemy",
  "other",
];

/** 门派 id → 颜色 (按 factions 数组顺序取色, 保证同一次渲染里稳定) */
export function makeFactionColorMap(factionIds: string[]): Map<string, string> {
  const map = new Map<string, string>();
  factionIds.forEach((id, index) => {
    map.set(id, FACTION_PALETTE[index % FACTION_PALETTE.length] ?? NEUTRAL_COLOR);
  });
  return map;
}

export interface GraphTheme {
  dark: boolean;
  /** 画布底色(导出 PNG 时用作 bg) */
  canvasBg: string;
  labelColor: string;
  labelOutline: string;
  accent: string;
}

/** 从 <html> 上的 dark/class 与 data-theme 推断当前主题(canvas 拿不到 CSS 变量) */
export function detectGraphTheme(): GraphTheme {
  const root = document.documentElement;
  if (typeof document === "undefined") {
    return { dark: false, canvasBg: "#fdfcf8", labelColor: "#3a3229", labelOutline: "#fdfcf8", accent: "#b3453a" };
  }
  const dark = root.classList.contains("dark");
  const themeName = root.dataset.theme ?? "";
  if (dark) {
    return {
      dark: true,
      canvasBg: "#252120",
      labelColor: "#e8e2da",
      labelOutline: "#252120",
      accent: "#d1735c",
    };
  }
  if (themeName === "sepia") {
    return { dark: false, canvasBg: "#f6eeda", labelColor: "#433422", labelOutline: "#f6eeda", accent: "#8a5a2b" };
  }
  if (themeName === "green") {
    return { dark: false, canvasBg: "#ecf5ee", labelColor: "#3c4b3f", labelOutline: "#ecf5ee", accent: "#3f7a55" };
  }
  return { dark: false, canvasBg: "#fdfcf8", labelColor: "#3a3229", labelOutline: "#fdfcf8", accent: "#b3453a" };
}

/** 标签字号: 普通 / 关键人物(加粗放大) —— 再乘以 labelScale(见 graphLabels.labelFontScale) */
export const LABEL_FONT = 12;
export const LABEL_FONT_STRONG = 15;

export interface StylesheetOptions {
  /** 缩放越小, 模型字号按比例放大, 让屏幕上的字保持可读(1 = 不放大) */
  labelScale?: number;
  /** 画布宽度(px): 窄屏(手机)下势力框标题降一档字号, 免得一个框里塞不下 */
  width?: number;
}

/** 窄屏阈值: 与 Tailwind 的 sm 断点一致 */
const NARROW_WIDTH = 640;

/** 势力框标题的基础字号(再乘 labelScale); 手机一档更小, 否则四个框的标题会顶出框外 */
export const FACTION_TITLE_FONT = { wide: 13, narrow: 11 } as const;

/**
 * 势力框标题的字号上限: 缩放补偿会把字号放大(手机 23% 时能到 44 模型 px), 窄框上就会撞到
 * text-max-width 被省略号吃掉人数(「未归属 · ... 」). 这里按框宽反算一个装得下的字号写回 data,
 * 样式表里 font-size 用 data(labelFont) —— 大框照样用缩放补偿的字号, 小框自动收一档.
 */
export function syncFactionTitleFont(cy: cytoscape.Core, labelScale: number): void {
  const scale = Math.max(1, labelScale);
  const narrow = cy.width() < NARROW_WIDTH;
  const compFont = (narrow ? FACTION_TITLE_FONT.narrow : FACTION_TITLE_FONT.wide) * scale;
  cy.nodes('[type="faction"]').forEach((node) => {
    const maxWidth = Number(node.data("labelMaxWidth") ?? 150);
    const text = String(node.data("label") ?? "");
    // 全角字宽约等于字号, 分隔符窄一点: 按 0.98 个字宽估, 留一点余量给省略判断
    const fitted = maxWidth / Math.max(4, text.length * 0.98);
    const font = Math.round(Math.min(compFont, Math.max(10, fitted)) * 10) / 10;
    if (node.data("labelFont") !== font) node.data("labelFont", font);
    // 实测标签框一半宽度 ≈ 0.8 em/字, 再加 8px 内边距 → 标题落在框内左上角
    const shift = Math.round(font * text.length * 0.8) + 8;
    if (node.data("titleShift") !== shift) node.data("titleShift", shift);
  });
}

/** 组装 cytoscape 样式表: 通用 → 形状 → 关系线型 → 交互态(class) */
export function buildGraphStylesheet(theme: GraphTheme, options: StylesheetOptions = {}): cytoscape.StylesheetJson {
  const scale = Math.max(1, options.labelScale ?? 1);
  const font = Math.round(LABEL_FONT * scale * 10) / 10;
  const fontStrong = Math.round(LABEL_FONT_STRONG * scale * 10) / 10;

  const rules: cytoscape.StylesheetJson = [
    {
      selector: "node",
      style: {
        "background-color": "data(color)",
        "background-opacity": 0.95,
        "border-width": 1.5,
        "border-color": "data(color)",
        shape: "ellipse",
        // 默认不画标签: 由 .gd-label / .gd-label-strong 分级打开(见 graphLabels)
        label: "",
        "font-size": font,
        "font-weight": 600,
        color: theme.labelColor,
        "text-outline-color": theme.labelOutline,
        "text-outline-width": 2.5,
        "text-valign": "bottom",
        "text-halign": "center",
        "text-margin-y": 5,
        "min-zoomed-font-size": 3,
        "overlay-opacity": 0,
        "transition-property": "opacity, border-width, border-color",
        "transition-duration": 160,
      },
    },
    // 节点大小 ∝ 关系数量; 只对有 size 数据的节点做映射(compound 父节点不适用)
    { selector: "node[size]", style: { width: "data(size)", height: "data(size)" } },
    // 身份形状: 主角 / 势力领袖 / 普通成员
    { selector: 'node[shape="star"]', style: { shape: "star" } },
    { selector: 'node[shape="round-rectangle"]', style: { shape: "round-rectangle" } },
    // compound 势力框: 淡底 + 虚线边 + 框内顶部标题(标题留白见 graphLayouts 的 FACTION_TITLE)
    {
      selector: 'node[type="faction"]',
      style: {
        shape: "round-rectangle",
        // 不用 data(bg): rgba 字符串在 canvas 上会被当成不透明色, 框一盖就把成员糊住了
        "background-color": "data(color)",
        "background-opacity": 0.07,
        "border-color": "data(color)",
        "border-width": 1.5,
        "border-style": "dashed",
        padding: "26px",
        label: "data(label)",
        // 由 syncFactionTitleFont 按「缩放补偿字号 与 框宽装得下的字号 取小」写进 data(labelFont):
        // 大框跟着缩放走, 窄框自动收一档, 免得人数被省略号吃掉
        "font-size": "data(labelFont)",
        "font-weight": 700,
        color: theme.labelColor,
        // 标题钉在框内左上角并按框宽截断: 居中时相邻框的标题会连成「伪齐 · 4 人岳家军 · 1 人」
        "text-valign": "top",
        "text-halign": "left",
        "text-wrap": "ellipsis",
        // 由 graphLayouts 按块宽写进 data(labelMaxWidth), 兜不住就省略号
        "text-max-width": "data(labelMaxWidth)",
        // 由 syncFactionTitleFont 按标题宽度算: compound 的 label 框是「居中贴在框左边缘」上的,
        // 不推半个标签宽, 390px 下标题有一半画到框外, 会压到左邻居的框、或被视口切掉
        // 类型定义里 text-margin-x 只收 number, 运行时支持 data() mapper, 这里绕过类型
        "text-margin-x": "data(titleShift)" as unknown as number,
        "text-margin-y": 8,
        "text-outline-width": 3,
        "min-zoomed-font-size": 5,
      },
    },
    {
      selector: "edge",
      style: {
        width: 1.5,
        "line-color": "#a8a29a",
        "curve-style": "bezier",
        "target-arrow-shape": "none",
        "source-arrow-shape": "none",
        "arrow-scale": 0.9,
        label: "",
        "font-size": 9,
        color: theme.labelColor,
        "text-outline-color": theme.labelOutline,
        "text-outline-width": 2.5,
        "text-rotation": "autorotate",
        "min-zoomed-font-size": 7,
        "overlay-opacity": 0,
        "transition-property": "opacity, line-color, width",
        "transition-duration": 160,
      },
    },
  ];

  // 关系线型编码: 一条规则一个类型, 避免 mapData 在多边形/枚举属性上的兼容问题
  for (const type of RELATION_ORDER) {
    const visual = RELATION_VISUAL[type];
    rules.push({
      selector: `edge[type="${type}"]`,
      style: {
        "line-color": visual.color,
        "line-style": visual.lineStyle,
        width: visual.width,
        "target-arrow-shape": visual.arrow ? "triangle" : "none",
        "target-arrow-color": visual.color,
      },
    });
  }

  // 绕行: 同层横边被中间节点的圆点挡住时, 由 graphEdgeRoutes 挂上这两个 class,
  // 换成带控制点的曲线从侧面绕过去(直线会从别人身上穿过去, 赵玖—吴夫人 被潘贤妃挡就是这种情况)
  rules.push(
    {
      selector: "edge.gd-detour-up",
      style: { "curve-style": "unbundled-bezier", "control-point-weights": 0.5, "control-point-distances": 64 },
    },
    {
      selector: "edge.gd-detour-down",
      style: { "curve-style": "unbundled-bezier", "control-point-weights": 0.5, "control-point-distances": -64 },
    },
  );

  // 交互态必须排在关系线型之后: cytoscape 里同特异性的规则后者胜, 否则选中高亮会被边类型规则盖掉
  rules.push(
    // 空间不够时只开一级标签, 放大后由 syncLabels 逐级放开
    {
      selector: "node.gd-label",
      style: {
        label: "data(label)",
        "font-size": font,
        "font-weight": 600,
      },
    },
    // 关键人物/选中/邻居: 更大更粗, fit 视图下也读得清
    {
      selector: "node.gd-label-strong",
      style: {
        label: "data(label)",
        "font-size": fontStrong,
        "font-weight": 700,
        "text-outline-width": 3.5,
      },
    },
    // 缩放够大时才显示边标签(见 GraphDemoPage 的 zoom 监听)
    { selector: "edge.gd-label", style: { label: "data(label)" } },
    // 势力框内的分段分隔线(见 graphLayouts.addFactionChunkRules): 只是装饰, 不吃事件
    {
      selector: 'node[type="rule"]',
      style: {
        shape: "round-rectangle",
        width: "data(w)",
        height: "data(h)",
        "background-color": theme.labelColor,
        "background-opacity": 0.34,
        "border-width": 0,
        label: "",
        events: "no",
      },
    },
    // 时间滑块: 未出现/已结束的元素直接不画
    { selector: ".gd-hidden", style: { display: "none" } },
    /**
     * 已故人物(lastSeen < 当前章, 见 graphElements.applyVisibility).
     * 世系里的老人死了也要留在树上, 只是淡一点: 最轻的一种区分 ——
     * 不换配色(门派色仍可读), 只用「淡填充 + 中性灰虚线圈」降低存在感.
     */
    {
      selector: "node.gd-deceased",
      style: {
        "background-opacity": 0.34,
        "border-color": NEUTRAL_COLOR,
        "border-style": "dashed",
        "border-width": 1.4,
      },
    },
    // 家族树视图的车外人员(见 graphElements.applyModeFilter)
    { selector: ".gd-mode-hidden", style: { display: "none" } },
    // 选中邻域之外压暗
    { selector: ".gd-dim", style: { opacity: 0.15 } },
    // 势力图里「势力内部」的关系: 打开开关时才画, 而且压得很淡(见 applyModeFilter)
    { selector: "edge.gd-intra-faint", style: { opacity: 0.22, width: 1.2 } },
    // 家族树里非血缘关系压暗, 让世系线自己浮出来
    { selector: "edge.gd-muted", style: { opacity: 0.14 } },
    // 搜索命中 / 点击选中. 光靠描边不行: 宋廷的节点色和 accent 几乎一样, 所以叠一圈发光底影
    {
      selector: "node.gd-focus",
      style: {
        "underlay-color": theme.accent,
        "underlay-opacity": 0.34,
        "underlay-padding": 10,
        "border-width": 3,
        "border-color": theme.labelColor,
      },
    },
    {
      selector: "node.gd-selected",
      style: {
        "underlay-color": theme.accent,
        "underlay-opacity": 0.28,
        "underlay-padding": 7,
        "border-width": 3,
        "border-color": theme.labelColor,
      },
    },
    { selector: "edge.gd-selected", style: { "line-color": theme.accent, width: 3, "line-style": "solid" } },
    // hover: 节点提亮 + 它的邻边压过 .gd-dim
    {
      selector: "node.gd-hover",
      style: {
        opacity: 1,
        "border-width": 3,
        "border-color": theme.accent,
        "underlay-color": theme.accent,
        "underlay-opacity": 0.16,
        "underlay-padding": 5,
      },
    },
    { selector: "edge.gd-hover", style: { opacity: 1, width: 3 } },
  );

  return rules;
}
