/**
 * graph-demo-v1 → cytoscape 元素.
 *
 * 三种视图共用一份节点/边, 只有「势力图」多一层 compound 父节点:
 *   tree / panorama → 扁平图
 *   faction        → 每个 faction 一个父节点, 成员作为子节点嵌进去
 */
import type cytoscape from "cytoscape";

import type { GraphDemoData } from "@/types/graphDemo";

import { NEUTRAL_COLOR, RELATION_VISUAL, makeFactionColorMap } from "./graphTheme";
import type { Visibility } from "./graphVisibility";

export type LayoutMode = "tree" | "faction" | "panorama";

export interface LayoutOption {
  id: LayoutMode;
  name: string;
  hint: string;
}

export const LAYOUT_OPTIONS: LayoutOption[] = [
  { id: "tree", name: "家族树", hint: "klay 分层, 按 generation 排行" },
  { id: "faction", name: "势力图", hint: "compound 父子分组, 按门派成块" },
  { id: "panorama", name: "全景", hint: "fcose 力导向, 冲突关系显色" },
];

/** 势力父节点 id 前缀, 避免和人物 id 撞车 */
const FACTION_NODE_PREFIX = "faction::";
/** 势力框标题的兜底最大宽度(px); 布局阶段按块宽覆盖 */
export const FACTION_LABEL_FALLBACK_WIDTH = 150;
/** 亲缘关系: 家族树视图只渲染这几类边上的人(见 graphLayouts.layoutFamilyTree) */
const KIN_RELATIONS = new Set(["parent", "spouse", "sibling"]);
/** 没有势力归属的人物在势力图里攒成的一块(不是数据里的势力, 不进图例) */
const UNASSIGNED_FACTION_ID = "__none";

export function factionNodeId(factionId: string): string {
  return `${FACTION_NODE_PREFIX}${factionId}`;
}

/** 身份 → 形状: 主角星形, 势力领袖圆角矩形, 其余椭圆 */
function shapeFor(role: string, isLeader: boolean): "star" | "round-rectangle" | "ellipse" {
  if (role.includes("主角") || role.includes("主人公")) return "star";
  if (isLeader) return "round-rectangle";
  return "ellipse";
}

export function buildGraphElements(
  data: GraphDemoData,
  mode: LayoutMode,
): cytoscape.ElementDefinition[] {
  const factionColor = makeFactionColorMap(data.factions.map((faction) => faction.id));
  const factionById = new Map(data.factions.map((faction) => [faction.id, faction]));

  const kinIds = new Set<string>();
  for (const relation of data.relations) {
    if (KIN_RELATIONS.has(relation.type)) {
      kinIds.add(relation.from);
      kinIds.add(relation.to);
    }
  }

  // 度数: 决定节点大小(人物关系越多越大)
  const degree = new Map<string, number>();
  for (const relation of data.relations) {
    degree.set(relation.from, (degree.get(relation.from) ?? 0) + 1);
    degree.set(relation.to, (degree.get(relation.to) ?? 0) + 1);
  }

  // 关键人物 = fit 视图下开名字的那一小撮(见 graphLabels 的 tier 0).
  // 预算写死 10 个: 之前是「关系数前 10 + 全部势力领袖 + 主角」, 势力一多(7 个)
  // 就膨胀到 17 个, 低缩放下自己的标签先叠成一团. 现在主角 + 最大的 4 个势力的
  // 领袖先占位, 剩下的名额按关系数补, 总数封顶 KEY_LABEL_BUDGET.
  const KEY_LABEL_BUDGET = 10;
  const factionSize = new Map<string, number>();
  for (const character of data.characters) {
    if (character.faction) factionSize.set(character.faction, (factionSize.get(character.faction) ?? 0) + 1);
  }
  const keyIds = new Set<string>();
  for (const character of data.characters) {
    if (character.role.includes("主角") || character.role.includes("主人公")) keyIds.add(character.id);
  }
  data.factions
    .filter((faction) => faction.leader)
    .sort((a, b) => (factionSize.get(b.id) ?? 0) - (factionSize.get(a.id) ?? 0))
    .slice(0, 4)
    .forEach((faction) => {
      if (faction.leader) keyIds.add(faction.leader);
    });
  [...degree.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .forEach(([id]) => {
      if (keyIds.size < KEY_LABEL_BUDGET) keyIds.add(id);
    });

  const elements: cytoscape.ElementDefinition[] = [];

  if (mode === "faction") {
    // 只给「确实有人物属于它」的势力建框, 空框没有信息量
    const memberCount = new Map<string, number>();
    for (const character of data.characters) {
      if (character.faction && factionById.has(character.faction)) {
        memberCount.set(character.faction, (memberCount.get(character.faction) ?? 0) + 1);
      }
    }
    // 没有势力归属的人物(宋金之外的散人)也攒成一块, 否则它们只能散在框外, 看着像布局出错
    const orphans = data.characters.filter(
      (character) => !character.faction || !factionById.has(character.faction),
    );
    if (orphans.length > 0) {
      elements.push({
        data: {
          id: factionNodeId(UNASSIGNED_FACTION_ID),
          label: `未归属 · ${orphans.length} 人`,
          type: "faction",
          color: NEUTRAL_COLOR,
          // 标题最大宽度(px): 布局阶段会按块宽覆盖, 这里只是兜底(见 graphLayouts.layoutFactionMap)
          labelMaxWidth: FACTION_LABEL_FALLBACK_WIDTH,
          // 标题字号: 首帧兜底值, 之后由 graphTheme.syncFactionTitleFont 按框宽/缩放改写
          labelFont: 26,
          titleShift: 8,
        },
      });
    }
    for (const faction of data.factions) {
      const count = memberCount.get(faction.id) ?? 0;
      if (count === 0) continue;
      const color = factionColor.get(faction.id) ?? NEUTRAL_COLOR;
      elements.push({
        data: {
          id: factionNodeId(faction.id),
          // 框标题 = 势力名 + 人数, 画在框内顶部(见 graphLayouts 的 FACTION_TITLE)
          label: `${faction.name} · ${count} 人`,
          type: "faction",
          color,
          // 同上: 兜底宽度, 真正按块宽算的值在布局阶段写入
          labelMaxWidth: FACTION_LABEL_FALLBACK_WIDTH,
          // 标题字号: 首帧兜底值, 之后由 graphTheme.syncFactionTitleFont 按框宽/缩放改写
          labelFont: 26,
          titleShift: 8,
        },
      });
    }
  }

  for (const character of data.characters) {
    const color = (character.faction && factionColor.get(character.faction)) || NEUTRAL_COLOR;
    const faction = character.faction ? factionById.get(character.faction) : undefined;
    const isLeader = faction?.leader === character.id;
    const weight = degree.get(character.id) ?? 0;
    const node: cytoscape.ElementDefinition = {
      data: {
        id: character.id,
        label: character.name,
        type: "character",
        color,
        shape: shapeFor(character.role, isLeader),
        size: 22 + Math.min(weight, 10) * 1.6,
        degree: weight,
        key: keyIds.has(character.id),
        kin: kinIds.has(character.id),
        factionId: character.faction ?? "",
        factionName: faction?.name ?? "无势力",
        generation: character.generation,
        firstSeen: character.firstSeen,
      },
    };
    if (mode === "faction") {
      node.data.parent =
        character.faction && faction
          ? factionNodeId(character.faction)
          : factionNodeId(UNASSIGNED_FACTION_ID);
    }
    elements.push(node);
  }

  const characterIds = new Set(data.characters.map((character) => character.id));
  const factionOf = new Map(data.characters.map((character) => [character.id, character.faction ?? ""]));

  data.relations.forEach((relation, index) => {
    // 自环和悬空引用在 demo 里没意义, 直接跳过
    if (relation.from === relation.to) return;
    if (!characterIds.has(relation.from) || !characterIds.has(relation.to)) return;
    const visual = RELATION_VISUAL[relation.type];
    const fromFaction = factionOf.get(relation.from) ?? "";
    const toFaction = factionOf.get(relation.to) ?? "";
    elements.push({
      data: {
        id: `rel-${index}`,
        source: relation.from,
        target: relation.to,
        type: relation.type,
        kind: "relation",
        label: relation.label ?? visual.name,
        from_chapter: relation.from_chapter,
        to_chapter: relation.to_chapter,
        quote: relation.quote ?? "",
        relationIndex: index,
        // 势力图默认只画跨势力关系(内部关系 200+ 条, 全画会糊成一片)
        sameFaction: fromFaction === toFaction,
      },
    });
  });

  return elements;
}

/** 家族树里的一个亲缘连通分量(父子/夫妻/手足连起来的一家人) */
export interface KinFamily {
  /** 分量 id: 成员 id 排序后取第一个, 稳定不会随渲染变 */
  id: string;
  /** 用分量里关系最多的人命名, 如「赵玖家」 */
  name: string;
  /** 成员人数 */
  size: number;
  memberIds: string[];
}

/**
 * 把亲缘关系切成一个一个「家」. 家族树视图全画时 83 人会散成 20 多条互不相连的小链,
 * fit 之后只有 57% 缩放, 谁也看不出结构; 有了这份清单, 工具栏就能让人一次只看一家,
 * 人少了缩放自然上去, 名字也就全显出来.
 */
export function kinFamilies(data: GraphDemoData): KinFamily[] {
  const parent = new Map<string, string>();
  const find = (id: string): string => {
    let root = id;
    while ((parent.get(root) ?? root) !== root) root = parent.get(root) ?? root;
    let cursor = id;
    while ((parent.get(cursor) ?? cursor) !== cursor) {
      const next = parent.get(cursor) ?? cursor;
      parent.set(cursor, root);
      cursor = next;
    }
    return root;
  };

  const degree = new Map<string, number>();
  for (const relation of data.relations) {
    degree.set(relation.from, (degree.get(relation.from) ?? 0) + 1);
    degree.set(relation.to, (degree.get(relation.to) ?? 0) + 1);
  }

  for (const relation of data.relations) {
    if (!KIN_RELATIONS.has(relation.type) || relation.from === relation.to) continue;
    for (const id of [relation.from, relation.to]) if (!parent.has(id)) parent.set(id, id);
    const rootA = find(relation.from);
    const rootB = find(relation.to);
    if (rootA !== rootB) parent.set(rootA, rootB);
  }

  const groups = new Map<string, string[]>();
  for (const id of parent.keys()) {
    const root = find(id);
    const bucket = groups.get(root) ?? [];
    bucket.push(id);
    groups.set(root, bucket);
  }

  const nameById = new Map(data.characters.map((character) => [character.id, character.name]));
  return [...groups.values()]
    .map((ids) => {
      const members = [...ids].sort();
      const top = [...members].sort(
        (a, b) => (degree.get(b) ?? 0) - (degree.get(a) ?? 0) || a.localeCompare(b),
      )[0] ?? "";
      return {
        id: members[0] ?? "",
        name: `${nameById.get(top) ?? top}家`,
        size: members.length,
        memberIds: members,
      };
    })
    .sort((a, b) => b.size - a.size || a.name.localeCompare(b.name));
}

/** 视图级过滤的开关(工具栏可调) */
export interface ViewFilterOptions {
  mode: LayoutMode;
  /** 全景: 少于这么多条关系的人物不画(默认 4, 冻结数据里 ≥4 的正好 51 人) */
  minDegree: number;
  /** 家族树: 只画这一个亲缘连通分量(null / 不给 = 全部); 见 kinFamilies() */
  treeFamilyIds?: Set<string> | null;
  /** 势力图: 是否连势力内部的关系一起画(默认关, 打开后转成很淡的线) */
  showIntraFaction: boolean;
}

/**
 * 视图级过滤: 决定「这个视图里画谁、画哪些边」. 用独立 class 而不是复用 gd-hidden ——
 * 那个是「读到第 N 章」的时间过滤, 两套口径会互相覆盖.
 *
 *   家族树: 只画有血缘/婚姻关系的人(106 个 0 代人物里绝大多数跟谁都没关系)
 *   势力图: 默认只画跨势力关系(内部关系 200+ 条, 全画就是一框红线)
 *   全景  : 按度数过滤, 太边缘的人物不铺进来
 */
export function applyModeFilter(cy: cytoscape.Core, options: ViewFilterOptions): void {
  const { mode, minDegree, showIntraFaction, treeFamilyIds } = options;
  // 家族树: 只有「这一刻还有亲缘边连着」的人才算一家人. 亲缘关系的章节区间会过期
  // (吴近 → 吴夫人 这类只出现一次的关系), 过期后节点自己还 visible, 却没人跟它连边,
  // 布局只排「有边的人」, 它们就会堆在 (0,0) 和赵玖叠在一起. 所以这里按可见亲缘边重算.
  // 必须先跑 applyVisibility(章节) 再跑这里.
  const kinLinked = new Set<string>();
  if (mode === "tree") {
    cy.edges().forEach((edge) => {
      if (edge.hasClass("gd-hidden")) return;
      if (!KIN_RELATIONS.has(String(edge.data("type")))) return;
      kinLinked.add(edge.source().id());
      kinLinked.add(edge.target().id());
    });
  }
  cy.batch(() => {
    cy.nodes().forEach((element) => {
      const node = element as cytoscape.NodeSingular;
      if (node.data("type") !== "character") return;
      let hidden = false;
      if (mode === "tree") {
        hidden =
          !node.data("kin") ||
          !kinLinked.has(node.id()) ||
          Boolean(treeFamilyIds && !treeFamilyIds.has(node.id()));
      } else if (mode === "panorama") hidden = Number(node.data("degree") ?? 0) < minDegree;
      node.toggleClass("gd-mode-hidden", hidden);
      node.removeClass("gd-intra-faint");
    });
    cy.edges().forEach((element) => {
      const edge = element as cytoscape.EdgeSingular;
      const hiddenEndpoint = edge.source().hasClass("gd-mode-hidden") || edge.target().hasClass("gd-mode-hidden");
      const type = String(edge.data("type"));
      const isKin = type === "parent" || type === "spouse" || type === "sibling";
      let hidden = hiddenEndpoint;
      if (mode === "tree" && !isKin) hidden = true;
      if (mode === "faction" && Boolean(edge.data("sameFaction")) && !showIntraFaction) hidden = true;
      edge.toggleClass("gd-mode-hidden", hidden);
      edge.toggleClass("gd-intra-faint", !hidden && mode === "faction" && Boolean(edge.data("sameFaction")));
    });
  });
}

/**
 * 把「读到第 N 章」的结果翻成 class: 不可见元素加 gd-hidden(样式表里 display: none).
 * 用 class 而不是删除元素, 这样拖滑块来回切不会反复重建图, 布局也能保留.
 */
export function applyVisibility(cy: cytoscape.Core, visibility: Visibility): void {
  cy.batch(() => {
    cy.nodes().forEach((element) => {
      const node = element as cytoscape.NodeSingular;
      const id = node.id();
      const isFactionNode = node.data("type") === "faction";
      const factionKey = id.slice(FACTION_NODE_PREFIX.length);
      const visible = isFactionNode
        ? factionKey === UNASSIGNED_FACTION_ID || visibility.factions.has(factionKey)
        : visibility.characters.has(id);
      node.toggleClass("gd-hidden", !visible);
    });
    cy.edges().forEach((element) => {
      const edge = element as cytoscape.EdgeSingular;
      const index = Number(edge.data("relationIndex"));
      edge.toggleClass("gd-hidden", !visibility.relations.has(index));
    });
  });
}
