/**
 * 三种布局的具体配置.
 *
 * tree     : cytoscape-klay 分层(方向 DOWN) + 按 generation 归行(见下方注释)
 * faction  : 每个势力一个 compound 父节点, 成员按「标签放得下」的网格排, 势力框打包成货架
 * panorama : cytoscape-fcose 力导向, 全图铺开看冲突关系
 *
 * 三种布局最后都自己调 fit(不用 layout 自带的 fit), 这样留白口径统一:
 * padding 要同时容下节点和它下方的标签, 否则 fit 之后最下面一行名字会被裁掉.
 */
import type cytoscape from "cytoscape";

import type { LayoutMode } from "./graphElements";

/** fit 留白(屏幕 px): 除了节点半径还要容下标签 + 描边, 窄屏收紧 */
export function fitPadding(cy: cytoscape.Core): number {
  const width = cy.width();
  if (width < 640) return 26;
  if (width < 1024) return 52;
  return 72;
}

/** 浮在画布上的卡片(标题卡/章节滑块)在画布坐标系里的矩形, 页面量好 DOM 后写进 cy.scratch("gd-occluders") */
export interface Occluder {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

/** 打包用的可用区域: 扣掉左上角标题卡那一带(和 fitToContent 选「卡片下方」时的口径保持一致) */
function usableCanvas(cy: cytoscape.Core): { width: number; height: number } {
  const pad = fitPadding(cy);
  const occluders = (cy.scratch("gd-occluders") as Occluder[] | undefined) ?? [];
  const bottomInset = Number(cy.scratch("gd-safe-bottom") ?? 0);
  const topInset =
    occluders.length > 0 ? Math.min(Math.max(...occluders.map((rect) => rect.y2)) + 14, cy.height() * 0.55) : 0;
  return {
    width: Math.max(cy.width() - pad * 2, 200),
    height: Math.max(cy.height() - pad * 2 - topInset - bottomInset, 200),
  };
}

/**
 * 统一出口: 只对人物 fit(compound 父节点包围盒要下一帧才更新), 并返回得到的缩放.
 * 不用 cy.fit: 左上角标题卡是浮在画布上的, cy.fit 会把内容铺到卡片底下 ——
 * 势力图里就是宋廷的标题和赵玖的标签被那张卡盖住. 这里在「卡片下方」和「卡片右侧」
 * 两块可用矩形里取能给出更大缩放的那块, 再把内容居中进去(既不会压在卡片上, 也不会被推出画布).
 */
export function fitToContent(cy: cytoscape.Core, eles: cytoscape.CollectionReturnValue): number {
  const targets = eles.nodes().filter((node) => node.data("type") === "character");
  const collection = targets.empty() ? eles.nodes() : (targets as unknown as cytoscape.CollectionReturnValue);
  if (collection.empty()) return cy.zoom();
  cy.resize();

  const box = collection.boundingBox({ includeLabels: true, includeOverlays: false });
  if (!Number.isFinite(box.w) || !Number.isFinite(box.h) || box.w <= 0 || box.h <= 0) {
    cy.fit(collection, fitPadding(cy));
    return cy.zoom();
  }

  // 势力图的框(compound 父节点)比人物包围盒更宽更高 —— 框宽来自货架打包的块宽, 成员只占其中
  // 一部分, 只按人物包围盒 fit 会把框边和框标题推到屏幕外(手机 390 下宋廷/金国的框左右都被切).
  // compound 包围盒要下一帧才更新, 这里按「成员 bbox + 框内边距」自己算, 结果稳定.
  const boxes = factionBoxes(eles);
  const headroom = boxes.length > 0 ? 8 : 0;
  const contentW = boxes.length > 0 ? Math.max(box.x2, ...boxes.map((b) => b.x2)) - Math.min(box.x1, ...boxes.map((b) => b.x1)) : box.w;
  const contentH = boxes.length > 0 ? Math.max(box.y2, ...boxes.map((b) => b.y2)) - Math.min(box.y1, ...boxes.map((b) => b.y1)) : box.h;
  const contentX1 = boxes.length > 0 ? Math.min(box.x1, ...boxes.map((b) => b.x1)) : box.x1;
  const contentY1 = boxes.length > 0 ? Math.min(box.y1, ...boxes.map((b) => b.y1)) : box.y1;
  const pad = fitPadding(cy);
  const canvasW = cy.width();
  const canvasH = cy.height();

  const bottomInset = Number(cy.scratch("gd-safe-bottom") ?? 0);
  let avail = { x1: pad, y1: pad, x2: canvasW - pad, y2: canvasH - pad - bottomInset };
  const occluders = (cy.scratch("gd-occluders") as Occluder[] | undefined) ?? [];
  const zoomOf = (rect: Occluder) =>
    Math.min((rect.x2 - rect.x1) / contentW, (rect.y2 - rect.y1) / contentH);
  if (occluders.length > 0 && canvasW > 0 && canvasH > 0) {
    const cardRight = Math.max(...occluders.map((rect) => rect.x2));
    const cardBottom = Math.max(...occluders.map((rect) => rect.y2));
    const GAP = 14;
    const below = {
      x1: pad,
      y1: Math.min(cardBottom + GAP, canvasH * 0.55),
      x2: canvasW - pad,
      y2: canvasH - pad - bottomInset,
    };
    // 「让到旁边」只在浮层确实是一条窄的左侧栏时才成立. 现在的 measureOccluders 登记的是
    // 一条横贯画布顶部的矩形(x1=0, x2=canvasW), 此时 cardRight === canvasW, 按比例算出的
    // beside 会被逼到右边 40%: 实测 1440×900 的全景只剩 0.55 缩放, 内容缩在右侧、左半屏全空
    // (静态 demo 因为包围盒比例不同恰好没触发). 所以窄栏之外只允许「让到下面」.
    if (cardRight < canvasW * 0.4) {
      const beside = {
        x1: Math.min(cardRight + GAP, canvasW * 0.6),
        y1: pad,
        x2: canvasW - pad,
        y2: canvasH - pad - bottomInset,
      };
      avail = zoomOf(below) >= zoomOf(beside) ? below : beside;
    } else {
      avail = below;
    }
  }

  const zoom = Math.max(Math.min(zoomOf(avail), cy.maxZoom()), 0.02);
  cy.zoom(zoom);
  cy.pan({
    x: avail.x1 + (avail.x2 - avail.x1 - contentW * zoom) / 2 - contentX1 * zoom,
    y: avail.y1 + (avail.y2 - avail.y1 - contentH * zoom) / 2 - (contentY1 - headroom) * zoom,
  });
  return zoom;
}

/**
 * 势力框的实体范围. 框宽由成员包围盒(含成员名字标签, 标签会往外溢出)撑开, 所以既要取 compound
 * 自己的 boundingBox(cytoscape 已经算上 padding 和成员标签), 也要取「成员 bbox + 内边距」兜底 ——
 * 标签是布局之后才挂上去的, 只信其中一个都会把框边估小, fit 完框就被屏幕边缘切掉.
 */
function factionBoxes(
  eles: cytoscape.CollectionReturnValue,
): { x1: number; y1: number; x2: number; y2: number }[] {
  const inset = FACTION_PAD + 2;
  const boxes: { x1: number; y1: number; x2: number; y2: number }[] = [];
  eles.nodes('[type="faction"]').forEach((parent) => {
    const kids = parent.children().filter((kid) => !kid.hasClass("gd-hidden") && !kid.hasClass("gd-mode-hidden"));
    if (kids.empty()) return;
    const fromKids = kids.boundingBox({ includeLabels: true, includeOverlays: false });
    // includeLabels: 框标题也算进内容范围, 否则 390px 下标题会被视口左边缘切掉
    const real = parent.boundingBox({ includeLabels: true, includeOverlays: false });
    boxes.push({
      x1: Math.min(fromKids.x1 - inset, real.x1),
      y1: Math.min(fromKids.y1 - inset, real.y1),
      x2: Math.max(fromKids.x2 + inset, real.x2),
      y2: Math.max(fromKids.y2 + inset, real.y2),
    });
  });
  return boxes;
}

/** 真正的「人物」节点: 排除 compound 父节点和势力图里的行分隔线 */
function characterNodes(eles: cytoscape.CollectionReturnValue): cytoscape.NodeSingular[] {
  return eles.nodes().filter((node) => node.data("type") === "character") as unknown as cytoscape.NodeSingular[];
}

/** 当前可见元素(时间滑块过滤掉的不参与布局, 否则隐藏节点会把可见节点挤开) */
export function visibleElements(cy: cytoscape.Core): cytoscape.CollectionReturnValue {
  return cy.elements().filter(
    (element) => !element.hasClass("gd-hidden") && !element.hasClass("gd-mode-hidden"),
  );
}

/** 家族树的分层间距 / 层内间距 */
const KIN_LAYER_GAP = 132;
const KIN_NODE_GAP = 128;
/** 亲缘块之间的空隙: 两个没关系的家族之间要留白, 否则看着像一家 */
const KIN_BLOCK_GAP = 208;

interface KinBlock {
  nodes: cytoscape.NodeSingular[];
  width: number;
  height: number;
  x: number;
  y: number;
}

/** 并查集: 把夫妻/手足并成「同一排里的一簇」 */
function makeUnionFind(): { find: (id: string) => string; union: (a: string, b: string) => void } {
  const parent = new Map<string, string>();
  const find = (id: string): string => {
    let root = parent.get(id) ?? id;
    if (root !== id) {
      root = find(root);
      parent.set(id, root);
    }
    return root;
  };
  return {
    find,
    union: (a, b) => {
      const ra = find(a);
      const rb = find(b);
      if (ra !== rb) parent.set(ra, rb);
    },
  };
}

/**
 * 一块连通亲缘子图的分层排布:
 *   - 代数用 parent 边做最长路径松弛(父上子下), 夫妻/手足再互相拉平到同一层;
 *   - 同一层里用并查集把夫妻/手足并成一簇, 簇按「父母的平均 x」排序(孩子落在父母下方),
 *     兄弟姐妹同代相邻、夫妻并排.
 * 不用 klay: klay 没有「同层约束」, 夫妻边会被当成父子边竖向摞起来(实测两人上下排队,
 * 看着像父子关系), 而 sibling 边会让整块被拉成长条.
 */
function layoutKinComponent(componentNodes: cytoscape.NodeSingular[], componentEdges: cytoscape.EdgeSingular[]): KinBlock {
  const depth = new Map<string, number>();
  componentNodes.forEach((node) => depth.set(node.id(), 0));
  const parentOf = new Map<string, string[]>();
  for (const edge of componentEdges) {
    if (String(edge.data("type")) !== "parent") continue;
    const from = edge.source().id();
    const to = edge.target().id();
    const list = parentOf.get(to);
    if (list) list.push(from);
    else parentOf.set(to, [from]);
  }

  // 松弛出代数: parent → +1, spouse/sibling → 拉平. 有环也不会发散(代数封顶)
  for (let iter = 0; iter < 40; iter += 1) {
    let changed = false;
    for (const edge of componentEdges) {
      const type = String(edge.data("type"));
      const a = edge.source().id();
      const b = edge.target().id();
      const da = depth.get(a) ?? 0;
      const db = depth.get(b) ?? 0;
      if (type === "parent") {
        const next = Math.min(da + 1, 12);
        if (db < next) {
          depth.set(b, next);
          changed = true;
        }
      } else {
        const merged = Math.min(Math.max(da, db), 12);
        if (da !== merged) {
          depth.set(a, merged);
          changed = true;
        }
        if (db !== merged) {
          depth.set(b, merged);
          changed = true;
        }
      }
    }
    if (!changed) break;
  }

  // 同层里夫妻/手足并簇
  const uf = makeUnionFind();
  for (const edge of componentEdges) {
    const type = String(edge.data("type"));
    if (type === "parent") continue;
    uf.union(edge.source().id(), edge.target().id());
  }

  const layers = new Map<number, cytoscape.NodeSingular[]>();
  for (const node of componentNodes) {
    const layer = depth.get(node.id()) ?? 0;
    const list = layers.get(layer);
    if (list) list.push(node);
    else layers.set(layer, [node]);
  }

  const placed = new Map<string, number>();
  let maxX = 0;
  let maxY = 0;
  // 某些块(比如只有夫妻两人、双方父母都不在图里)松弛后最低代可能不是 0, 归一化到 0,
  // 否则整块会被顶到下面留出一大片空白
  const minLayer = Math.min(...layers.keys());
  for (const rawLayer of [...layers.keys()].sort((a, b) => a - b)) {
    const layer = rawLayer - minLayer;
    const list = layers.get(layer) ?? [];
    const clusters = new Map<string, cytoscape.NodeSingular[]>();
    for (const node of list) {
      const key = uf.find(node.id());
      const bucket = clusters.get(key);
      if (bucket) bucket.push(node);
      else clusters.set(key, [node]);
    }
    // 簇的排序键: 簇内成员的父母水平位置均值(没父母就排到右边, 保持稳定)
    const ordered = [...clusters.values()].map((members) => {
      const xs: number[] = [];
      for (const member of members) {
        for (const parentId of parentOf.get(member.id()) ?? []) {
          const x = placed.get(parentId);
          if (x !== undefined) xs.push(x);
        }
      }
      const anchor = xs.length > 0 ? xs.reduce((sum, x) => sum + x, 0) / xs.length : Number.MAX_SAFE_INTEGER;
      return { members, anchor };
    });
    ordered.sort((a, b) => a.anchor - b.anchor || (a.members[0]?.id() ?? "").localeCompare(b.members[0]?.id() ?? ""));

    let cursorX = 0;
    const y = layer * KIN_LAYER_GAP;
    for (const cluster of ordered) {
      for (const node of cluster.members) {
        node.position({ x: cursorX, y });
        placed.set(node.id(), cursorX);
        cursorX += KIN_NODE_GAP;
        maxX = Math.max(maxX, cursorX - KIN_NODE_GAP);
      }
    }
    maxY = Math.max(maxY, y);
  }

  return { nodes: componentNodes, width: maxX, height: maxY, x: 0, y: 0 };
}

/** 亲缘块按货架打包: 一列一列塞, 行宽按画布长宽比挑, 免得几十个两块之家排成一条 5000px 的长龙 */
function packKinBlocks(blocks: KinBlock[], usableWidth: number, usableHeight: number): void {
  const candidates = [700, 900, 1100, 1300, 1500, 1800, 2200, 2800, 3600];
  let best: { rowWidth: number; score: number } | null = null;
  for (const rowWidth of candidates) {
    let cursorX = 0;
    let cursorY = 0;
    let rowHeight = 0;
    let totalWidth = 0;
    for (const block of blocks) {
      if (cursorX > 0 && cursorX + block.width > rowWidth) {
        cursorY += rowHeight + KIN_BLOCK_GAP;
        cursorX = 0;
        rowHeight = 0;
      }
      cursorX += block.width + KIN_BLOCK_GAP;
      rowHeight = Math.max(rowHeight, block.height);
      totalWidth = Math.max(totalWidth, cursorX);
    }
    const totalHeight = cursorY + rowHeight;
    const score = Math.min(usableWidth / Math.max(totalWidth, 1), usableHeight / Math.max(totalHeight, 1));
    if (!best || score > best.score) best = { rowWidth, score };
  }
  const rowWidth = best?.rowWidth ?? 1500;

  let cursorX = 0;
  let cursorY = 0;
  let rowHeight = 0;
  for (const block of blocks) {
    if (cursorX > 0 && cursorX + block.width > rowWidth) {
      cursorY += rowHeight + KIN_BLOCK_GAP;
      cursorX = 0;
      rowHeight = 0;
    }
    block.x = cursorX;
    block.y = cursorY;
    cursorX += block.width + KIN_BLOCK_GAP;
    rowHeight = Math.max(rowHeight, block.height);
  }
}

/**
 * 家族树 = 亲缘子图(只算 parent/spouse/sibling 三类边).
 *
 * 不按 generation 归行了: 冻结数据里 0 代有 106 人, 其中绝大多数跟谁都没有亲缘关系,
 * 按代排只会得到一张 106 人的大栅格 —— 树完全看不出来. 现在只画亲缘子图
 * (其余人在这个视图里 display:none, 切回去还在, 见 applyModeFilter).
 */
export function layoutFamilyTree(cy: cytoscape.Core, eles: cytoscape.CollectionReturnValue): number {
  const nodes = characterNodes(eles);
  if (nodes.length === 0) return cy.zoom();

  const KIN_TYPES = new Set(["parent", "spouse", "sibling"]);
  const kinEdges = eles.edges().filter((edge) => KIN_TYPES.has(String(edge.data("type"))));
  const subgraph = kinEdges.connectedNodes().union(kinEdges) as cytoscape.CollectionReturnValue;
  if (subgraph.empty()) return fitToContent(cy, eles);

  const blocks: KinBlock[] = [];
  const components = subgraph.components() as unknown as cytoscape.CollectionReturnValue;
  components.forEach((component) => {
    const compNodes = component.nodes().toArray() as cytoscape.NodeSingular[];
    const compEdges = component.edges().toArray() as cytoscape.EdgeSingular[];
    if (compNodes.length === 0) return;
    blocks.push(layoutKinComponent(compNodes, compEdges));
  });
  blocks.sort((a, b) => b.nodes.length - a.nodes.length || b.height - a.height);

  const usable = usableCanvas(cy);
  packKinBlocks(blocks, usable.width, usable.height);

  cy.batch(() => {
    for (const block of blocks) {
      const dx = block.x;
      const dy = block.y;
      block.nodes.forEach((node) => {
        node.position({ x: node.position("x") + dx, y: node.position("y") + dy });
      });
    }
  });

  return fitToContent(cy, eles);
}

// ---------------------------------------------------------------------------
// 势力图
// ---------------------------------------------------------------------------

/**
 * 成员间距按「节点 + 下方标签」定:
 *   横向 96 → 名字(最多 4 个汉字 ≈ 52px @13px)之间留得住白;
 *   纵向 76 → 节点(≤38px) + 10px 标签间距 + 标签高度(≈17px) 之后还剩余量.
 */
const FACTION_MEMBER_GAP_X = 104;
const FACTION_MEMBER_GAP_Y = 84;
/** 单个势力框最多几列(超过就折行, 8~10 列时框最接近「横向卡片」) */
const FACTION_MAX_COLS_CANDIDATES = [6, 7, 8, 9, 10, 11, 12, 14, 16];
/** 势力框之间的间距 */
const FACTION_BLOCK_GAP = 56;
/**
 * 块宽额外余量: compound 框的实际宽度 = 成员 bbox + padding, 而成员名字是画在圆点边上、
 * 会伸出去一截(缩放补偿后模型字号能有 30px, 4 字名字比 28px 的点宽出 ~80), 不留这点余量
 * 框就会比打包预留的块宽更宽, 相邻框叠在一起.
 */
const FACTION_LABEL_MARGIN = 48;
/** compound 框的内边距(与样式表里的 padding 对齐) */
const FACTION_PAD = 26;
/** 框内顶部留给标题(势力名 + 人数)的高度 */
const FACTION_TITLE = 32;
/**
 * 每 4 行打断一次 (加一条额外空隙 + 一道极淡分隔线).
 * 一个框里 20~130 个成员时, 纯等距栅格糊成一片「点阵」, 分段之后才有行/组的感觉.
 */
const FACTION_CHUNK_ROWS = 4;
const FACTION_CHUNK_GAP = 34;

/** 第 row 行的纵向偏移(含分段空隙) */
function rowOffsetY(row: number): number {
  return row * FACTION_MEMBER_GAP_Y + Math.floor(row / FACTION_CHUNK_ROWS) * FACTION_CHUNK_GAP;
}
/** 一行最多铺多宽(候选值, 运行时按画布长宽比挑一个最省空间的) */
const FACTION_ROW_WIDTH_CANDIDATES = [1100, 1220, 1340, 1480, 1600, 1800];

interface FactionBlock {
  /** null = 无势力人物那一堆(没有 compound 父节点) */
  parent: cytoscape.NodeSingular | null;
  members: cytoscape.NodeSingular[];
  cols: number;
  rows: number;
  width: number;
  height: number;
  /** 打包后落到货架上的位置(块左上角) */
  x: number;
  y: number;
}

function maxNodeSize(members: cytoscape.NodeSingular[]): number {
  let size = 0;
  for (const member of members) {
    size = Math.max(size, Number(member.data("size") ?? 26));
  }
  return size;
}

function makeBlock(
  parent: cytoscape.NodeSingular | null,
  members: cytoscape.NodeSingular[],
  maxCols: number,
): FactionBlock {
  const count = Math.max(1, members.length);
  // 稍微偏横向: 势力框做成卡片, 一行能并排几个比竖条好认
  const cols = Math.min(maxCols, Math.max(1, Math.ceil(Math.sqrt(count * 1.6))));
  const rows = Math.ceil(count / cols);
  const size = maxNodeSize(members);
  const width = (cols - 1) * FACTION_MEMBER_GAP_X + size + FACTION_PAD * 2 + FACTION_LABEL_MARGIN;
  const height = rowOffsetY(rows - 1) + size + FACTION_PAD * 2 + FACTION_TITLE;
  return { parent, members, cols, rows, width, height, x: 0, y: 0 };
}

/**
 * 货架打包: 顺序不变地往当前行塞, 放不下就换行.
 * 返回整块的包围尺寸(用于挑参数).
 */
function shelfPack(blocks: FactionBlock[], rowWidth: number): { width: number; height: number } {
  let cursorX = 0;
  let cursorY = 0;
  let rowHeight = 0;
  let maxWidth = 0;
  for (const block of blocks) {
    if (cursorX > 0 && cursorX + block.width > rowWidth) {
      cursorY += rowHeight + FACTION_BLOCK_GAP;
      cursorX = 0;
      rowHeight = 0;
    }
    block.x = cursorX;
    block.y = cursorY;
    cursorX += block.width + FACTION_BLOCK_GAP;
    rowHeight = Math.max(rowHeight, block.height);
    maxWidth = Math.max(maxWidth, block.x + block.width);
  }
  return { width: maxWidth, height: cursorY + rowHeight };
}

/**
 * 势力图.
 *
 * 不用 klay 的 layoutHierarchy: 同一势力的成员在层级上完全同级, klay 会把 44 个成员
 * 摞成一列(实测框被拉成 2200px 高的窄条, 缩到 33% 才看得见).
 * 改成自己打包: 每个势力内部按「标签放得下」的网格排 → 量出卡片尺寸 →
 * 按货架摆到画布上, compound 父节点的包围盒由 cytoscape 依子节点自动撑开.
 *
 * 列数上限 / 行宽不写死: 拿画布长宽比挑一组让 fit 缩放最大的参数(手机窄高屏自然换一套).
 */
export function layoutFactionMap(cy: cytoscape.Core, eles: cytoscape.CollectionReturnValue): number {
  const nodes = eles.nodes();
  if (nodes.length === 0) return cy.zoom();

  const parents = nodes.filter((node) => node.data("type") === "faction");
  // 没有 parent 的才是「无势力」人物(compound 子节点不能重复摆)
  const orphans = nodes.filter((node) => node.data("type") === "character" && node.parent().empty());
  // 上一轮画的分隔线重画之前先清掉(同一个 cy 会在三种视图间复用)
  cy.nodes('[type="rule"]').remove();

  // 成员按关系数排序: 核心人物落在卡片左上角, 一眼能看到
  const byDegree = (a: cytoscape.NodeSingular, b: cytoscape.NodeSingular): number =>
    Number(b.data("degree") ?? 0) - Number(a.data("degree") ?? 0) || a.id().localeCompare(b.id());

  const groups: { parent: cytoscape.NodeSingular | null; members: cytoscape.NodeSingular[] }[] = [];
  for (const parent of parents) {
    const members = parent.children().nodes().toArray() as cytoscape.NodeSingular[];
    if (members.length === 0) continue;
    members.sort(byDegree);
    groups.push({ parent, members });
  }
  // 大块优先: 让 44 人的宋廷先落位, 小卡片再填空隙
  groups.sort((a, b) => b.members.length - a.members.length);
  if (orphans.length > 0) {
    const list = orphans.toArray() as cytoscape.NodeSingular[];
    list.sort(byDegree);
    groups.push({ parent: null, members: list });
  }

  const usable = usableCanvas(cy);
  const usableWidth = usable.width;
  const usableHeight = usable.height;

  let best: { cols: number; rowWidth: number; blocks: FactionBlock[]; score: number } | null = null;
  for (const maxCols of FACTION_MAX_COLS_CANDIDATES) {
    for (const rowWidth of FACTION_ROW_WIDTH_CANDIDATES) {
      const blocks = groups.map((group) => makeBlock(group.parent, group.members, maxCols));
      const box = shelfPack(blocks, rowWidth);
      // 目标: 让 fit 之后的缩放尽量大(即模型尺寸相对可用区域尽量小)
      const score = Math.min(usableWidth / Math.max(box.width, 1), usableHeight / Math.max(box.height, 1));
      if (!best || score > best.score) best = { cols: maxCols, rowWidth, blocks, score };
    }
  }
  const blocks = best?.blocks ?? groups.map((group) => makeBlock(group.parent, group.members, 8));

  cy.batch(() => {
    for (const block of blocks) {
      const originX = block.x + FACTION_PAD;
      const originY = block.y + FACTION_PAD + FACTION_TITLE;
      // 标题只许在框内一行: 最大宽度按块宽算(留出左右各 8px 的 text-margin-x), 超了就省略号.
      // 不这么做的话, 窄屏上相邻框的标题会连成「伪齐 · 4 人岳家军 · 1 人」
      block.parent?.data("labelMaxWidth", Math.max(48, block.width - FACTION_PAD * 2 - 16));
      block.members.forEach((member, index) => {
        const col = index % block.cols;
        const row = Math.floor(index / block.cols);
        member.position({
          x: originX + col * FACTION_MEMBER_GAP_X,
          y: originY + rowOffsetY(row),
        });
      });
    }
  });

  addFactionChunkRules(cy, blocks);

  return fitToContent(cy, eles);
}

/**
 * 势力框内的分段分隔线: 在每 4 行之间的额外空隙里画一道极淡的横线.
 * 不用 cytoscape 的 compound 做「行组」(嵌套 compound 会连累 padding / 拖拽 / 层级),
 * 一条线就够表达分段.
 */
function addFactionChunkRules(cy: cytoscape.Core, blocks: FactionBlock[]): void {
  for (const block of blocks) {
    if (block.rows <= FACTION_CHUNK_ROWS) continue;
    const originY = block.y + FACTION_PAD + FACTION_TITLE;
    const originX = block.x + FACTION_PAD;
    const width = (block.cols - 1) * FACTION_MEMBER_GAP_X + 24;
    const centerX = originX + ((block.cols - 1) * FACTION_MEMBER_GAP_X) / 2;
    const key = block.parent?.id() ?? "free";
    for (let row = FACTION_CHUNK_ROWS; row < block.rows; row += FACTION_CHUNK_ROWS) {
      cy.add({
        group: "nodes",
        classes: "gd-rule",
        position: {
          x: centerX,
          y: originY + rowOffsetY(row) - FACTION_MEMBER_GAP_Y / 2 - FACTION_CHUNK_GAP / 2,
        },
        data: { id: `rule-${key}-${row}`, type: "rule", w: width, h: 3 },
        selectable: false,
        grabbable: false,
        pannable: false,
        locked: true,
      });
    }
  }
}

// ---------------------------------------------------------------------------
// 全景
// ---------------------------------------------------------------------------

/** djb2: 同一个 id 永远得到同一个数 —— 全景初始坐标靠它算, 不用 Math.random */
function hashId(text: string): number {
  let hash = 5381;
  for (let i = 0; i < text.length; i += 1) hash = ((hash << 5) + hash + text.charCodeAt(i)) >>> 0;
  return hash;
}

/**
 * 全景的初始坐标: 黄金角螺旋(点再多也不会挤在同一点) + 按 id 哈希的微扰.
 *
 * fcose 在 randomize:false 时完全按初始位置迭代, 所以初始位置必须是确定的:
 * 同一份数据两次渲染 → 同样的图; 只有换视图/点重排才重新跑.
 * 节点按 id 排序后再摆, 与数据里的顺序无关(后端换个顺序也不会让全景变样).
 */
function seedPanoramaPositions(nodes: cytoscape.NodeSingular[]): void {
  const ordered = [...nodes].sort((a, b) => a.id().localeCompare(b.id()));
  const goldenAngle = Math.PI * (3 - Math.sqrt(5));
  const step = 48;
  ordered.forEach((node, index) => {
    // 0~1 的确定性抖动: 打散螺旋的规整感, 又不会让两次渲染不同
    const jitter = (hashId(node.id()) % 997) / 997;
    const angle = index * goldenAngle + jitter * 0.9;
    const radius = step * Math.sqrt(index + 1);
    node.position({ x: Math.cos(angle) * radius, y: Math.sin(angle) * radius });
  });
}

/** 全景: fcose 力导向, 让敌对/同盟关系的极性一眼看出来 */
export function layoutPanorama(cy: cytoscape.Core, eles: cytoscape.CollectionReturnValue): number {
  if (eles.empty()) return cy.zoom();
  // 确定性: 固定的初始坐标 + randomize:false + 固定 randomSeed
  // (以前 randomize:true, 每次刷新节点位置全变、缩放 74%~79% 浮动)
  seedPanoramaPositions(eles.nodes().toArray() as cytoscape.NodeSingular[]);
  eles.layout({
    name: "fcose",
    // 必须是 false: fcose 带 animate 时 run() 立即返回, 此时节点还在原位,
    // 随后的 fit 会按「一个点」算包围盒 → 缩放顶到 maxZoom(实测 300%)
    animate: false,
    // 自己 fit: 统一留白口径(要含标签)
    fit: false,
    quality: "default",
    // 不随机初始化: 从上面算出的稳定坐标开始迭代, 同一份数据每次结果一致
    randomize: false,
    randomSeed: 1,
    // 0 度/小分量的节点很多, 排斥力给太大会把整张图推到 2600×2000 以外(fit 后只剩 0.28,
    // 字全部读不出来); 1.2e5 是实测 135 人/269 关系下「节点不重叠且 fit 后仍有 0.5」的档位.
    nodeRepulsion: 120000,
    idealEdgeLength: 110,
    nodeSeparation: 220,
    gravity: 0.05,
    gravityRange: 3.8,
    numIter: 2500,
    // 必须显式给: fcose 的默认分量间距很小, 几十个小分量会被挨着平铺,
    // 整图被压成一坨(fit 顶到 maxZoom)
    packComponents: true,
    componentSpacing: 130,
    // 简易避让: 节点带标签/尺寸差异, 打开后不容易糊成一团
    uniformNodeDimensions: false,
    nodeDimensionsIncludeLabels: false,
  } as cytoscape.LayoutOptions).run();

  return fitToContent(cy, eles);
}

export function runLayout(
  cy: cytoscape.Core,
  mode: LayoutMode,
  eles: cytoscape.CollectionReturnValue = visibleElements(cy),
): number {
  // 面板/图例的开合会改变容器尺寸, 不先同步的话随后的 fit 会拿旧宽高算缩放
  cy.resize();
  if (mode === "tree") return layoutFamilyTree(cy, eles);
  if (mode === "faction") return layoutFactionMap(cy, eles);
  return layoutPanorama(cy, eles);
}
