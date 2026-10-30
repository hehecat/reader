/**
 * 家族树(谱系树)布局 —— 纯计算, 零 cytoscape 依赖.
 *
 * 为什么不用 barycenter 排序(旧 layoutKinComponent 的做法):
 *   旧算法把每一代单独拿出来, 按「簇内成员父母的平均 x」排序, 然后 cursorX 从 0 重新往右摞.
 *   层与层之间没有「带(band)」的概念, 于是父代和子代的 x 各排各的 —— 一个第 3 代的人和
 *   它第 1 代的父亲之间可以差 4~5 个格位, 连线就变成跨好几行的长斜线, 整张图看着像乱麻.
 *
 * 现在的做法(Reingold–Tilford / tidy-tree 的简化版, 全部自底向上一次算完):
 *   1. 代数: 用 parent 边做最长路径松弛(父上子下), spouse/sibling 互相拉平, 封顶 12 防环.
 *   2. 再把父代「下沉」到紧贴子代(min(子代代数) - 1), 固定点迭代 —— 数据里 田芸/柳柔绚/万萧华
 *      这种「本人是根, 但孩子被手足边拉深了两代」的人, 不下沉就会吊在很高的地方, 拉出长斜线.
 *   3. 夫妻并成「夫妻单元」(横向并排, 间距 1 格), 兄弟单元按 id 稳定排序成一条有序的孩子列表.
 *   4. tidy 分配横向槽位: 后序遍历, 子树先各占一段连续区间(兄弟子树之间留 siblingGap 个空槽),
 *      父单元取「第一个孩子中心 ~ 最后一个孩子中心」的中点 → 父永远居中于自己的子代跨度.
 *      因为区间是后序分配的, 同一块里不同分支的区间天然互不相交(不会交织), 边只会斜一点点.
 *   5. 每个连通块独立算完再按货架打包: 人数最多的块排最前(左上), 块之间留 blockGap.
 *
 * 确定性: 所有排序键最终都落到 id 的码点比较, 与入参顺序、locale 无关 → 同输入两次逐位相同.
 */

/** 入参只吃最朴素的结构: {id} 和 {source,target,type}, 方便单测和复用 */
export interface KinTreeInputNode {
  id: string;
}

export interface KinTreeInputEdge {
  source: string;
  target: string;
  type: string;
}

export interface KinTreeOptions {
  /** 一格 = 同代相邻单元(以及夫妻两人)的横向间距 px */
  cell?: number;
  /** 代数之间的行距 px */
  rowGap?: number;
  /** 兄弟子树之间空几个格位 */
  siblingGap?: number;
  /** 代数松弛上限(防环) */
  maxGeneration?: number;
  /** 块与块之间的留白 px */
  blockGap?: number;
  /** 每个货架左侧留给「第 N 代」标尺的沟宽 px */
  rulerGutter?: number;
  /** 货架最大宽度 px: 超过就换行, 免得十几个小块排成一条 5000px 的长龙(不填则按 viewport 自动挑) */
  shelfWidth?: number;
  /** 画布可用区域: 有它就能按「fit 后缩放最大」挑货架宽度; 不给就用默认 3600 */
  viewport?: { width: number; height: number };
}

/** 货架打包结果(纯几何, 方便按视口挑一格宽度) */
interface ShelfPlan {
  items: { block: KinTreeBlock; x: number; y: number }[];
  shelves: { y: number; rows: number; left: number }[];
  width: number;
  height: number;
}

export interface KinTreeBlock {
  /** 块内人物 id(升序) */
  ids: string[];
  /** 块在画布上的左上角 */
  x: number;
  y: number;
  /** 块的包围盒尺寸: 单列块也给一格宽, 否则打包时会和邻块贴在一起 */
  width: number;
  height: number;
  /** 这个块占了几代(行数) */
  rows: number;
}

/**
 * 装饰层上的一枚标签(不参与人数/家数/搜索, 也不进图数据).
 * 主块的行标「第 N 代」(generation 是 0 起的, 显示时 +1); 其它块不编号, 只标一次「独立家系」.
 */
export interface KinTreeRulerRow {
  x: number;
  y: number;
  generation: number;
  /** 直接给渲染层用的文案, 纯函数算好, 保证同输入逐位相同 */
  text: string;
}

export interface KinTreeLayout {
  positions: Map<string, { x: number; y: number }>;
  /** 代数(块内归零的行号): 同一块里同代数的人 y 完全相同 */
  generation: Map<string, number>;
  blocks: KinTreeBlock[];
  rows: KinTreeRulerRow[];
}

/** 横向一格: 同层单元间距 ≥180, 夫妻并排也正好一格 */
export const KIN_CELL = 140;
/** 行距 ≥150 */
export const KIN_ROW_GAP = 168;
/** 块间留白: 没关系的家族要隔开, 否则看着像一家 */
export const KIN_BLOCK_GAP = 260;
/** 每个货架左侧留给「第 N 代」标尺的空白(模型单位): 标尺是 DOM 浮层, 字号不随缩放变小
 *  —— 不留这条沟, 缩到 0.37 时标尺会正好压在整棵树最左边那个人身上 */
export const KIN_RULER_GUTTER = 120;
/** 非主块的标签: 跟主家没有代际关系, 不能跟着挂「第 N 代」 */
export const KIN_DETACHED_LABEL = "独立家系";
/** 「独立家系」标签相对块左边缘往左让出的模型距离(块间距 260, 让 96 不会压到邻块) */
export const KIN_DETACHED_LABEL_OFFSET = 96;
/** 「独立家系」标签再往上提一点: 人名的标签画在节点下方, 提到行上方才不会压到第一个人的名字 */
export const KIN_DETACHED_LABEL_LIFT = 60;
/** 代数松弛上限 */
export const KIN_MAX_GENERATION = 12;
/** 没有视口信息时的货架宽度 */
export const KIN_SHELF_WIDTH = 3600;
/** 挑货架宽度时试的候选值 */
const SHELF_CANDIDATES = [1400, 1800, 2200, 2800, 3600, 4400, 5200];

const KIN_TYPES = new Set(["parent", "spouse", "sibling"]);

/** 码点比较: 不用 localeCompare, 结果与 locale 无关 */
function cmpId(a: string, b: string): number {
  if (a < b) return -1;
  if (a > b) return 1;
  return 0;
}

/** 数组首个元素: tsconfig 开了 noUncheckedIndexedAccess, 这里把「一定非空」显式表达出来 */
function head<T>(list: readonly T[]): T {
  const value = list[0];
  if (value === undefined) throw new Error("kin layout: 空数组取首个");
  return value;
}

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

interface RowCluster {
  /** 该行簇(夫妻+手足并起来)里的所有人, id 升序 */
  members: string[];
  generation: number;
}

interface TreeUnit {
  key: string;
  members: string[];
  clusterId: string;
}

export function computeKinTreeLayout(
  nodes: KinTreeInputNode[],
  edges: KinTreeInputEdge[],
  opts: KinTreeOptions = {},
): KinTreeLayout {
  const cell = opts.cell ?? KIN_CELL;
  const rowGap = opts.rowGap ?? KIN_ROW_GAP;
  const siblingGap = opts.siblingGap ?? 1;
  const maxGen = opts.maxGeneration ?? KIN_MAX_GENERATION;
  const blockGap = opts.blockGap ?? KIN_BLOCK_GAP;
  const rulerGutter = opts.rulerGutter ?? KIN_RULER_GUTTER;

  const nodesById = new Map<string, KinTreeInputNode>();
  for (const node of nodes) nodesById.set(node.id, node);

  // 只留亲缘边, 去掉悬空边和自环; 排序 + 去重, 让后续遍历与入参顺序无关
  const seen = new Set<string>();
  const kinEdges: { source: string; target: string; type: string }[] = [];
  for (const edge of edges) {
    const type = String(edge.type);
    if (!KIN_TYPES.has(type)) continue;
    const source = String(edge.source);
    const target = String(edge.target);
    if (source === target) continue;
    if (!nodesById.has(source) || !nodesById.has(target)) continue;
    const key = `${type}\u0000${source}\u0000${target}`;
    if (seen.has(key)) continue;
    seen.add(key);
    kinEdges.push({ source, target, type });
  }
  kinEdges.sort((a, b) => cmpId(a.source, b.source) || cmpId(a.target, b.target) || cmpId(a.type, b.type));

  const positions = new Map<string, { x: number; y: number }>();
  const generation = new Map<string, number>();
  const blocks: KinTreeBlock[] = [];
  const rulers: KinTreeRulerRow[] = [];
  if (kinEdges.length === 0) return { positions, generation, blocks, rows: rulers };

  // --- 连通块: 只有被亲缘边连着的人才进树(孤立人物在 tree 模式里本来就被隐藏)
  const linked = new Set<string>();
  for (const edge of kinEdges) {
    linked.add(edge.source);
    linked.add(edge.target);
  }
  const componentUf = makeUnionFind();
  const linkedIds = [...linked].sort(cmpId);
  for (const id of linkedIds) componentUf.union(id, id);
  for (const edge of kinEdges) componentUf.union(edge.source, edge.target);
  const groups = new Map<string, string[]>();
  for (const id of linkedIds) {
    const root = componentUf.find(id);
    const bucket = groups.get(root);
    if (bucket) bucket.push(id);
    else groups.set(root, [id]);
  }
  const components = [...groups.values()]
    .map((ids) => [...ids].sort(cmpId))
    .sort((a, b) => b.length - a.length || cmpId(head(a), head(b)));

  for (const ids of components) {
    const inBlock = new Set(ids);
    const blockEdges = kinEdges.filter((edge) => inBlock.has(edge.source) && inBlock.has(edge.target));
    const laid = layoutBlock(ids, blockEdges, { cell, rowGap, siblingGap, maxGen });
    blocks.push(laid.block);
    for (const [id, point] of laid.points) positions.set(id, point);
    for (const [id, gen] of laid.generations) generation.set(id, gen);
  }

  // --- 货架打包: 人数多的块在前(左上), 每行宽度不超过 rowWidth
  const planShelves = (rowWidth: number): ShelfPlan => {
    const items: ShelfPlan["items"] = [];
    const shelves: ShelfPlan["shelves"] = [];
    let cursorX = 0;
    let shelfY = 0;
    let shelfHeight = 0;
    let shelfRows = 0;
    let shelfLeft = 0;
    let maxRight = 0;
    let open = false;
    for (const block of blocks) {
      const width = Math.max(block.width, cell);
      if (open && cursorX + blockGap + width > rowWidth) {
        shelves.push({ y: shelfY, rows: shelfRows, left: shelfLeft });
        shelfY += shelfHeight + blockGap;
        cursorX = 0;
        shelfHeight = 0;
        shelfRows = 0;
        open = false;
      }
      // 每行第一个块让开标尺沟
      const x = open ? cursorX + blockGap : rulerGutter;
      // 标尺锚在最左边(沟里), 不是第一个块上
      if (!open) shelfLeft = 0;
      items.push({ block, x, y: shelfY });
      // 同一货架上的块顶边对齐 → 同一行的「第 N 代」对这一货架的所有块同时成立
      cursorX = x + width;
      maxRight = Math.max(maxRight, cursorX);
      // 单行块(尤其只有一行的小家)高度是 0, 这里至少占一行, 免得上一个货架的行压到下一个货架上
      shelfHeight = Math.max(shelfHeight, block.height, rowGap);
      shelfRows = Math.max(shelfRows, block.rows);
      open = true;
    }
    shelves.push({ y: shelfY, rows: shelfRows, left: shelfLeft });
    return { items, shelves, width: Math.max(maxRight, cell), height: shelfY + shelfHeight };
  };

  // 货架宽度的挑法: 让 fit 之后的缩放尽量大(宽高比接近视口), 免得十几个两块之家排成一条长龙
  const viewport = opts.viewport;
  let plan: ShelfPlan;
  if (typeof opts.shelfWidth === "number" && opts.shelfWidth > 0) {
    plan = planShelves(Math.max(opts.shelfWidth, cell));
  } else if (viewport && viewport.width > 0 && viewport.height > 0) {
    let best: { plan: ShelfPlan; score: number } | null = null;
    for (const candidate of SHELF_CANDIDATES) {
      const attempt = planShelves(Math.max(candidate, cell));
      const score = Math.min(viewport.width / attempt.width, viewport.height / attempt.height);
      if (!best || score > best.score + 1e-9) best = { plan: attempt, score };
    }
    plan = best?.plan ?? planShelves(KIN_SHELF_WIDTH);
  } else {
    plan = planShelves(KIN_SHELF_WIDTH);
  }

  for (const item of plan.items) {
    item.block.x = item.x;
    item.block.y = item.y;
    item.block.width = Math.max(item.block.width, cell);
  }
  // 只有主块(人数最多, 排在最前)挂「第 N 代」标尺: 其它块跟主家没有代际关系,
  // 同一高度挂上「第 1 代」会被读成跟主家同代.
  const main = head(blocks);
  for (let row = 0; row < main.rows; row += 1) {
    rulers.push({ x: main.x - rulerGutter, y: main.y + row * rowGap, generation: row, text: `第 ${row + 1} 代` });
  }
  // 其它块各自是独立家系: 只在块的左上角标一次, 不带代数
  for (const block of blocks) {
    if (block === main) continue;
    rulers.push({
      x: Math.max(block.x - KIN_DETACHED_LABEL_OFFSET, 0),
      y: block.y - KIN_DETACHED_LABEL_LIFT,
      generation: 0,
      text: KIN_DETACHED_LABEL,
    });
  }
  rulers.sort((a, b) => a.y - b.y || a.x - b.x || cmpId(a.text, b.text));

  // 块内坐标是相对块左上角的, 这里平移成画布坐标
  for (const block of blocks) {
    for (const id of block.ids) {
      const point = positions.get(id);
      if (!point) continue;
      point.x += block.x;
      point.y += block.y;
    }
  }
  return { positions, generation, blocks, rows: rulers };
}

function layoutBlock(
  ids: string[],
  blockEdges: { source: string; target: string; type: string }[],
  cfg: { cell: number; rowGap: number; siblingGap: number; maxGen: number },
): {
  block: KinTreeBlock;
  points: Map<string, { x: number; y: number }>;
  generations: Map<string, number>;
} {
  const { cell, rowGap, siblingGap, maxGen } = cfg;

  // --- 1) 代数: parent 边 = 最长路径松弛(父上子下), spouse/sibling 互相拉平, 封顶防环
  const depth = new Map<string, number>();
  for (const id of ids) depth.set(id, 0);
  for (let iter = 0; iter < 40; iter += 1) {
    let changed = false;
    for (const edge of blockEdges) {
      const da = depth.get(edge.source) ?? 0;
      const db = depth.get(edge.target) ?? 0;
      if (edge.type === "parent") {
        // 父代 → 子代 +1. 环里反复 +1 会被 maxGen 卡住, 不会发散
        const next = Math.min(da + 1, maxGen);
        if (db < next) {
          depth.set(edge.target, next);
          changed = true;
        }
      } else {
        // 夫妻/手足同代: 两端都取较大值(取小会让子代反过来比父代小)
        const merged = Math.min(Math.max(da, db), maxGen);
        if (da !== merged) {
          depth.set(edge.source, merged);
          changed = true;
        }
        if (db !== merged) {
          depth.set(edge.target, merged);
          changed = true;
        }
      }
    }
    if (!changed) break;
  }

  // --- 2) 行簇: 夫妻 + 手足并成一簇(同代同排). 簇代数取成员最小深度, 再让父簇下沉贴住子簇
  const clusterUf = makeUnionFind();
  for (const id of ids) clusterUf.union(id, id);
  for (const edge of blockEdges) if (edge.type !== "parent") clusterUf.union(edge.source, edge.target);
  const clusterMembers = new Map<string, string[]>();
  for (const id of ids) {
    const root = clusterUf.find(id);
    const bucket = clusterMembers.get(root);
    if (bucket) bucket.push(id);
    else clusterMembers.set(root, [id]);
  }
  const clusters: RowCluster[] = [];
  for (const members of clusterMembers.values()) {
    members.sort(cmpId);
    clusters.push({ members, generation: Math.min(...members.map((id) => depth.get(id) ?? 0)) });
  }
  clusters.sort((a, b) => cmpId(head(a.members), head(b.members)));
  const clusterById = new Map<string, RowCluster>();
  for (const cluster of clusters) clusterById.set(clusterUf.find(head(cluster.members)), cluster);

  const childClusters = new Map<string, Set<string>>();
  for (const cluster of clusters) childClusters.set(clusterUf.find(head(cluster.members)), new Set());
  for (const edge of blockEdges) {
    if (edge.type !== "parent") continue;
    const from = clusterUf.find(edge.source);
    const to = clusterUf.find(edge.target);
    if (from !== to) childClusters.get(from)?.add(to);
  }
  /**
   * 下沉: 一个簇如果有孩子, 就往下贴到「最浅的那个孩子头上」(min(子代代数) - 1).
   * 每轮只会让簇更深, 且被 maxGen 卡住 → 固定点迭代一定停.
   * 这是长斜线的第二个来源: 旧代码里 田芸/柳柔绚/万萧华 这种「自己是根, 孩子却被手足边拉深两代」
   * 的人停在最上面那一排, 跟孩子的连线硬生生跨了两三行.
   */
  for (let iter = 0; iter < 40; iter += 1) {
    let changed = false;
    for (const cluster of clusters) {
      const kids = childClusters.get(clusterUf.find(head(cluster.members)));
      if (!kids || kids.size === 0) continue;
      let shallowest = Number.POSITIVE_INFINITY;
      for (const kid of kids) {
        const child = clusterById.get(kid);
        if (child) shallowest = Math.min(shallowest, child.generation);
      }
      if (!Number.isFinite(shallowest)) continue;
      const desired = Math.min(shallowest - 1, maxGen);
      if (desired > cluster.generation) {
        cluster.generation = desired;
        changed = true;
      }
    }
    if (!changed) break;
  }

  // --- 3) 夫妻单元 = 树节点(横向并排, 间距一格)
  const spouseUf = makeUnionFind();
  for (const id of ids) spouseUf.union(id, id);
  for (const edge of blockEdges) if (edge.type === "spouse") spouseUf.union(edge.source, edge.target);
  const unitMembers = new Map<string, string[]>();
  for (const id of ids) {
    const root = spouseUf.find(id);
    const bucket = unitMembers.get(root);
    if (bucket) bucket.push(id);
    else unitMembers.set(root, [id]);
  }
  // 单元内部排序: 有配偶的人尽量坐中间, 这样「一个人两任配偶」时两条 spouse 边都只差一格
  const spouseNeighbors = new Map<string, Set<string>>();
  for (const edge of blockEdges) {
    if (edge.type !== "spouse") continue;
    for (const [id, other] of [
      [edge.source, edge.target],
      [edge.target, edge.source],
    ] as const) {
      const bucket = spouseNeighbors.get(id);
      if (bucket) bucket.add(other);
      else spouseNeighbors.set(id, new Set([other]));
    }
  }
  const orderUnitMembers = (members: string[]): string[] => {
    if (members.length <= 2) return [...members].sort(cmpId);
    const degree = (id: string) => spouseNeighbors.get(id)?.size ?? 0;
    const rest = new Set(members);
    const byDegree = (a: string, b: string): number => degree(b) - degree(a) || cmpId(a, b);
    // 起点: 配偶最多的人(同数取 id 小者)
    const start = head([...members].sort(byDegree));
    const out: string[] = [start];
    rest.delete(start);
    while (rest.size > 0) {
      const last = head(out.slice(-1));
      const neighbor = [...(spouseNeighbors.get(last) ?? [])].filter((id) => rest.has(id)).sort(byDegree)[0];
      const next = neighbor ?? head([...rest].sort(cmpId));
      out.push(next);
      rest.delete(next);
    }
    // 链是从「配偶最多的人」长出来的, 转一圈把 TA 转到正中间 —— 两任配偶才能一人一边各差一格
    let hubIndex = 0;
    for (const [index, id] of out.entries()) {
      if (byDegree(id, head(out.slice(hubIndex))) < 0) hubIndex = index;
    }
    const target = Math.floor((out.length - 1) / 2);
    const shift = ((hubIndex - target) % out.length + out.length) % out.length;
    return shift === 0 ? out : [...out.slice(shift), ...out.slice(0, shift)];
  };

  const units: TreeUnit[] = [];
  for (const [key, members] of unitMembers) {
    const ordered = orderUnitMembers(members);
    units.push({ key, members: ordered, clusterId: clusterUf.find(head(ordered)) });
  }
  units.sort((a, b) => cmpId(head(a.members), head(b.members)));
  const unitOf = (id: string): string => spouseUf.find(id);

  // --- 4) 层级: 每个单元挂到「子单元最多」的父单元下(多父时选主家, 同数按 id)
  const parentUnitsOf = new Map<string, Set<string>>();
  for (const unit of units) parentUnitsOf.set(unit.key, new Set());
  for (const edge of blockEdges) {
    if (edge.type !== "parent") continue;
    const from = unitOf(edge.source);
    const to = unitOf(edge.target);
    if (from !== to) parentUnitsOf.get(to)?.add(from);
  }
  const childCount = new Map<string, number>();
  for (const unit of units) childCount.set(unit.key, 0);
  for (const parents of parentUnitsOf.values()) {
    for (const parent of parents) childCount.set(parent, (childCount.get(parent) ?? 0) + 1);
  }
  const childUnits = new Map<string, string[]>();
  for (const unit of units) childUnits.set(unit.key, []);
  const hasParent = new Set<string>();
  for (const unit of units) {
    const parents = [...(parentUnitsOf.get(unit.key) ?? [])].sort(cmpId);
    if (parents.length === 0) continue;
    let primary = head(parents);
    for (const parent of parents) {
      const a = childCount.get(parent) ?? 0;
      const b = childCount.get(primary) ?? 0;
      if (a > b || (a === b && cmpId(parent, primary) < 0)) primary = parent;
    }
    childUnits.get(primary)?.push(unit.key);
    hasParent.add(unit.key);
  }
  for (const kids of childUnits.values()) kids.sort(cmpId);

  // 根: 没有父单元的; 有环时 DFS 走不到的那些单元也各当一棵小树的根, 保证所有人都被摆出来
  const roots: string[] = units.filter((unit) => !hasParent.has(unit.key)).map((unit) => unit.key).sort(cmpId);
  const reached = new Set<string>();
  const walk = (key: string): void => {
    if (reached.has(key)) return;
    reached.add(key);
    for (const kid of childUnits.get(key) ?? []) walk(kid);
  };
  for (const root of roots) walk(root);
  for (const unit of units) {
    if (reached.has(unit.key)) continue;
    reached.add(unit.key);
    roots.push(unit.key);
    walk(unit.key);
  }
  roots.sort(cmpId);

  // --- 5) tidy 分配槽位: 后序算子树区间, 父取子代跨度中点; 前序落绝对槽位
  const shape = new Map<string, { center: number; extent: number; kidOffsets: number[] }>();
  const measure = (key: string): void => {
    const kids = childUnits.get(key) ?? [];
    const kidOffsets: number[] = [];
    let cursor = 0;
    let right = 0;
    let firstCenter = 0;
    let lastCenter = 0;
    kids.forEach((kid, index) => {
      measure(kid);
      const kidShape = shape.get(kid);
      if (!kidShape) return;
      kidOffsets.push(cursor);
      if (index === 0) firstCenter = cursor + kidShape.center;
      lastCenter = cursor + kidShape.center;
      right = cursor + kidShape.extent;
      cursor = right + siblingGap;
    });
    const width = unitMembers.get(key)?.length ?? 1;
    const half = (width - 1) / 2;
    const center = kids.length > 0 ? (firstCenter + lastCenter) / 2 : 0;
    const left = Math.min(0, center - half);
    const rightEdge = Math.max(right, center + half);
    const shift = -left;
    shape.set(key, { center: center + shift, extent: rightEdge + shift, kidOffsets });
  };
  for (const root of roots) measure(root);

  const unitX = new Map<string, number>();
  const place = (key: string, base: number): void => {
    const unitShape = shape.get(key);
    if (!unitShape) return;
    unitX.set(key, base + unitShape.center);
    const kids = childUnits.get(key) ?? [];
    kids.forEach((kid, index) => place(kid, base + (unitShape.kidOffsets[index] ?? 0)));
  };
  let cursor = 0;
  for (const root of roots) {
    place(root, cursor);
    cursor += (shape.get(root)?.extent ?? 0) + siblingGap;
  }
  const extentTotal = Math.max(cursor - siblingGap, 0);

  // --- 6) 出坐标: 夫妻两人以单元中心对称展开(间距一格), y 按代数排
  const points = new Map<string, { x: number; y: number }>();
  const generations = new Map<string, number>();
  let minGeneration = Number.POSITIVE_INFINITY;
  let maxRow = 0;
  for (const unit of units) {
    const cluster = clusterById.get(unit.clusterId);
    const gen = cluster ? cluster.generation : 0;
    minGeneration = Math.min(minGeneration, gen);
    for (const id of unit.members) generations.set(id, gen);
  }
  if (!Number.isFinite(minGeneration)) minGeneration = 0;
  for (const unit of units) {
    const center = unitX.get(unit.key) ?? 0;
    const width = unit.members.length;
    unit.members.forEach((id, index) => {
      const gen = generations.get(id) ?? 0;
      const row = gen - minGeneration;
      maxRow = Math.max(maxRow, row);
      points.set(id, { x: (center + index - (width - 1) / 2) * cell, y: row * rowGap });
    });
  }

  return {
    block: {
      ids: [...ids],
      x: 0,
      y: 0,
      width: extentTotal * cell,
      height: maxRow * rowGap,
      rows: maxRow + 1,
    },
    points,
    generations: new Map([...generations].map(([id, gen]) => [id, gen - minGeneration])),
  };
}
