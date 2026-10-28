/**
 * 标签排布: 「度数优先 + 贪心去重叠」, 能放多少放多少.
 *
 * 以前按「缩放相对 fit」分三级, fit 视图下只开 8~10 个关键人物的名字, 结果是 73% 缩放下
 * 51 个点只有 9 个名字, 画布大片留白却不肯写字. 现在改成: 所有可见人物都进候选, 按
 * 「选中/hover 的邻域 → 关键人物 → 关系数」排队, 逐个放, 放之前测一次包围盒是否压住已放下的
 * 标签, 压住就跳过(让给后面更重要的). 于是标签数量随空间自然变化: fit 时能放多少放多少,
 * 放大后自然全开, 窄屏自动收敛.
 *
 * 上限只是兜底, 不是主要手段:
 *   labelBudget    全图最多几个 (家族树 ≤40 人时 = 全开, 全景 24, 势力图 60)
 *   perGroupBudget 同一个势力框内最多几个 (势力图 12, 免得宋廷框里一屏都是字)
 *
 * 选中或 hover 的邻域永远保留(strong 更粗更大), 且不参与去重叠 —— 用户点谁就得看见谁.
 */
import type cytoscape from "cytoscape";

import { LABEL_FONT, LABEL_FONT_STRONG } from "./graphTheme";

/** 屏幕上希望达到的字号(px), 用于换算模型字号 */
const TARGET_SCREEN_FONT = 11;
/** 字号补偿上限: 手机全景能缩到 0.16, 再往上翻也救不回来, 4.2 屏上约 9px */
const MAX_LABEL_SCALE = 4.2;
/** 窄屏上不铺满: 390 宽时十几个名字必然互相压, 交给去重叠收敛到这个数 */
const NARROW_WIDTH = 640;
const NARROW_BUDGET = 10;

/** 字号补偿系数: 缩到 0.3 时模型字号放大到 3.2 倍(上限), 屏幕上仍 ≈10px */
export function labelFontScale(cy: cytoscape.Core): number {
  const zoom = Math.max(cy.zoom(), 0.05);
  const scale = TARGET_SCREEN_FONT / (LABEL_FONT * zoom);
  return Math.min(MAX_LABEL_SCALE, Math.max(1, Math.round(scale * 20) / 20));
}

export interface SyncLabelsOptions {
  /** 选中/hover 的节点集合: 它们和自己的一跳邻居强制显示 strong 标签 */
  focusNodes?: cytoscape.NodeCollection | null;
  /** 全图最多放几个标签(兜底上限, 999 = 全开) */
  labelBudget?: number;
  /** 单个 compound 框内最多放几个(势力图用) */
  perGroupBudget?: number;
}

interface Candidate {
  id: string;
  priority: number;
  strong: boolean;
  group: string;
  w: number;
  h: number;
  x: number;
  y: number;
}

/**
 * 把「放得下的标签」写成 class. 每次 zoom / 选中变化后调用一次, 一两百个节点开销可忽略.
 */
export function syncLabels(cy: cytoscape.Core, options: SyncLabelsOptions = {}): void {
  const { focusNodes, perGroupBudget = 999 } = options;
  const narrow = cy.width() < NARROW_WIDTH;
  const budget = Math.max(options.labelBudget ?? 999, 1);
  const effectiveBudget = narrow ? Math.min(budget, NARROW_BUDGET) : budget;

  const forced = new Set<string>();
  if (focusNodes && !focusNodes.empty()) {
    focusNodes.forEach((node) => {
      forced.add(node.id());
    });
    focusNodes.neighborhood().nodes().forEach((node) => {
      forced.add(node.id());
    });
  }

  // 去重叠一律在「屏幕坐标」里算: 字号×缩放 才是眼睛看到的像素, 再留一点点安全边
  // (描边 2.5~3.5px 会把文字撑宽, 离线测 bbox 时才发现差这几个像素就压上了).
  const zoom = Math.max(cy.zoom(), 0.02);
  const scale = labelFontScale(cy) * zoom;
  // 关键人物用 .gd-label-strong, 字号大 25%(LABEL_FONT_STRONG 15 vs LABEL_FONT 12) ——
  // 按 12 估宽会漏掉这几个正好压上去的
  const fontPlain = LABEL_FONT * scale;
  const fontStrong = LABEL_FONT_STRONG * scale;
  const MARGIN_X = 5;
  const MARGIN_Y = 2;
  const candidates: Candidate[] = [];

  cy.batch(() => {
    cy.nodes().forEach((element) => {
      const node = element as cytoscape.NodeSingular;
      // compound 势力框的标题是常开的, 不参与排布
      if (node.data("type") !== "character") return;
      if (node.hasClass("gd-hidden") || node.hasClass("gd-mode-hidden")) {
        node.removeClass("gd-label gd-label-strong");
        return;
      }

      const text = String(node.data("label") ?? "");
      if (!text) return;
      const degree = Number(node.data("degree") ?? 0);
      const isFocus = forced.has(node.id());
      const isKey = Boolean(node.data("key"));
      const parent = node.parent();
      const group = parent.empty() ? "" : parent.first().id();
      const point = node.renderedPosition();
      const strong = isFocus || isKey;
      const fontPx = strong ? fontStrong : fontPlain;
      candidates.push({
        id: node.id(),
        priority: isFocus ? 1000 : isKey ? 500 + degree : degree,
        strong,
        group,
        // CJK 名字的字宽 ≈ 字号本身
        w: Math.max(text.length, 1) * fontPx * 1.06 + MARGIN_X,
        h: fontPx * 1.35 + MARGIN_Y,
        x: point.x,
        // 标签画在节点正下方(text-valign: bottom, text-margin-y: 5)
        y: point.y + fontPx * 0.55,
      });
    });
  });

  candidates.sort((a, b) => b.priority - a.priority);

  const kept: Candidate[] = [];
  const groupCount = new Map<string, number>();
  for (const candidate of candidates) {
    if (candidate.priority < 1000) {
      if (kept.length >= effectiveBudget) break;
      if ((groupCount.get(candidate.group) ?? 0) >= perGroupBudget) continue;
      const collides = kept.some(
        (other) =>
          Math.abs(other.x - candidate.x) < (other.w + candidate.w) / 2 &&
          Math.abs(other.y - candidate.y) < (other.h + candidate.h) / 2,
      );
      if (collides) continue;
    }
    kept.push(candidate);
    groupCount.set(candidate.group, (groupCount.get(candidate.group) ?? 0) + 1);
  }

  const keep = new Set(kept.map((candidate) => candidate.id));
  const strongIds = new Set(kept.filter((candidate) => candidate.strong).map((candidate) => candidate.id));

  cy.batch(() => {
    cy.nodes().forEach((element) => {
      const node = element as cytoscape.NodeSingular;
      if (node.data("type") !== "character") return;
      const show = keep.has(node.id());
      node.toggleClass("gd-label", show && !strongIds.has(node.id()));
      node.toggleClass("gd-label-strong", show && strongIds.has(node.id()));
    });
  });

  cy.scratch("gd-label-count", keep.size);
}

/** 边标签的超参: 每种关系最多 2 个, 且不许压在节点上 */
export interface SyncEdgeLabelsOptions {
  /** 每种关系类型最多画几个标签 */
  perTypeBudget?: number;
  /** 缩放到这个级别以下就不画边标签 */
  minZoom?: number;
}

/** 边标签字号的基准(见 graphTheme 里 edge 的 font-size: 9) */
const EDGE_LABEL_FONT = 9;

/**
 * 边标签: 只画「放得下且不重复」的那几个.
 *
 * 以前的规则是放大到 1.05 就把所有边的名字全打开, 结果单家视图里「夫妻」正好压在中间那位
 * 妻子的圆点上(半截字透过节点), 「亲缘」还重复出现四次. 现在:
 *   1. 每种关系类型最多 perTypeBudget 个, 优先度数高的那对;
 *   2. 标签包围盒只要压住任何一个可见人物节点, 就整个不画(空间让给节点本身).
 * 剩下的信息交给图例和右侧面板, 不靠图上堆字.
 */
export function syncEdgeLabels(cy: cytoscape.Core, options: SyncEdgeLabelsOptions = {}): number {
  const perTypeBudget = options.perTypeBudget ?? 2;
  const minZoom = options.minZoom ?? 1.05;
  const zoom = cy.zoom();

  if (zoom < minZoom) {
    cy.edges().removeClass("gd-label");
    cy.scratch("gd-edge-label-count", 0);
    return 0;
  }

  // 人物的包围盒: 边标签压到谁身上就不画(CJK 名字宽扁, 留 1px 余量)
  const nodeBoxes: { x1: number; y1: number; x2: number; y2: number }[] = [];
  const hiddenIds = new Set<string>();
  cy.nodes().forEach((element) => {
    const node = element as cytoscape.NodeSingular;
    if (node.hasClass("gd-hidden") || node.hasClass("gd-mode-hidden")) {
      hiddenIds.add(node.id());
      return;
    }
    // 势力框会把整张图包进去, 不能算遮挡
    if (node.data("type") !== "character") return;
    const box = node.renderedBoundingBox({ includeLabels: false, includeOverlays: false });
    nodeBoxes.push({ x1: box.x1, y1: box.y1, x2: box.x2, y2: box.y2 });
  });

  const candidates: { id: string; type: string; priority: number; box: Box }[] = [];
  cy.edges().forEach((element) => {
    const edge = element as cytoscape.EdgeSingular;
    const text = String(edge.data("label") ?? "");
    if (!text) return;
    if (edge.hasClass("gd-hidden") || edge.hasClass("gd-mode-hidden")) return;
    if (hiddenIds.has(edge.source().id()) || hiddenIds.has(edge.target().id())) return;

    // 标签画在边的中点, 跟着线旋转: 用旋转后的外接矩形近似, 宁可估大不估小
    const from = edge.source().renderedPosition();
    const to = edge.target().renderedPosition();
    const w = Math.max(text.length, 1) * EDGE_LABEL_FONT * zoom * 1.06 + 4;
    const h = EDGE_LABEL_FONT * zoom * 1.4 + 2;
    const dx = to.x - from.x;
    const dy = to.y - from.y;
    const len = Math.hypot(dx, dy) || 1;
    const cos = Math.abs(dx) / len;
    const sin = Math.abs(dy) / len;
    const halfW = (w * cos + h * sin) / 2;
    const halfH = (w * sin + h * cos) / 2;
    const cx = (from.x + to.x) / 2;
    const cyMid = (from.y + to.y) / 2;

    candidates.push({
      id: edge.id(),
      type: String(edge.data("type") ?? ""),
      priority: Number(edge.source().data("degree") ?? 0) + Number(edge.target().data("degree") ?? 0),
      box: { x1: cx - halfW, y1: cyMid - halfH, x2: cx + halfW, y2: cyMid + halfH },
    });
  });

  candidates.sort((a, b) => b.priority - a.priority);

  const overlaps = (a: Box, b: Box) => a.x1 < b.x2 && b.x1 < a.x2 && a.y1 < b.y2 && b.y1 < a.y2;
  const typeCount = new Map<string, number>();
  const placed: Box[] = [];
  const keep = new Set<string>();
  for (const candidate of candidates) {
    if ((typeCount.get(candidate.type) ?? 0) >= perTypeBudget) continue;
    if (nodeBoxes.some((nodeBox) => overlaps(nodeBox, candidate.box))) continue;
    if (placed.some((box) => overlaps(box, candidate.box))) continue;
    keep.add(candidate.id);
    placed.push(candidate.box);
    typeCount.set(candidate.type, (typeCount.get(candidate.type) ?? 0) + 1);
  }

  cy.batch(() => {
    cy.edges().forEach((element) => {
      element.toggleClass("gd-label", keep.has(element.id()));
    });
  });
  cy.scratch("gd-edge-label-count", keep.size);
  return keep.size;
}

/** 清掉标签 class(重建图/换视图时用) */
export function clearLabels(cy: cytoscape.Core): void {
  cy.nodes().removeClass("gd-label gd-label-strong");
  cy.edges().removeClass("gd-label");
}

interface Box {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}
