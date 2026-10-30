/**
 * 时间维度: 「读到第 N 章」时哪些人物/关系/势力还在场上.
 *
 * 纯函数, 方便页面在拖动滑块时只做一次计算, 再把结果翻成 cytoscape class.
 */
import type { GraphDemoData, GraphRelation } from "@/types/graphDemo";

/**
 * 亲缘关系: 家族树只画这几类边上的人(亲子 / 夫妻 / 手足).
 *
 * 单一来源: graphElements 的视图过滤、页面的家族分组、单测都从这里取,
 * 免得三处各写一份类型表后口径走偏.
 */
export const KIN_RELATION_TYPES: ReadonlySet<string> = new Set(["parent", "spouse", "sibling"]);

/**
 * 永久血缘: 亲子 / 手足 —— 不认 `to_chapter` 上界.
 *
 * 数据抽取把「最后一次被提到 / 人物死亡」写进了关系的 to_chapter
 * (`李木田 → 李长湖 [parent] 4..1711`、`李木田 → 李项平 [sibling] 2..1711`).
 * 若当过期处理, 第 1749 章时李木田失去全部亲缘边, 家族树按「有可见亲缘边」取人时
 * 就会把他整块删掉, 族人被切成互不相连的几簇, 而面板还在说「1 家」.
 * 父子和手足是永久事实(出族、断绝亲子这类极端剧情不在本口径内), 所以只看 from_chapter.
 */
export const PERMANENT_KIN_RELATION_TYPES: ReadonlySet<string> = new Set(["parent", "sibling"]);

export function isKinRelation(type: string): boolean {
  return KIN_RELATION_TYPES.has(type);
}

/**
 * 这条关系在第 N 章还成不成立.
 *
 * - `from_chapter > chapter`: 还没发生, 一律不可见.
 * - `to_chapter`: 只有「真的会变」的关系才吃这个上界(夫妻离异、反目、同盟破裂),
 *   血缘见 PERMANENT_KIN_RELATION_TYPES, 只认起始章.
 */
export function isRelationVisibleAt(relation: GraphRelation, chapter: number): boolean {
  if (relation.from_chapter > chapter) return false;
  if (PERMANENT_KIN_RELATION_TYPES.has(relation.type)) return true;
  // to_chapter 为 null 表示「至今仍在」
  return relation.to_chapter === null || relation.to_chapter >= chapter;
}

/**
 * 人物是否「已故」: lastSeen(最后一次出现) 早于当前章节.
 *
 * 已故的人不会从家族树里消失 —— 血缘边不按 to_chapter 过期(见 isRelationVisibleAt),
 * 世系节点必须留着, 只是画淡一点(gd-deceased, 见 graphTheme). lastSeen 为 null 表示全书都在场.
 */
export function isDeceasedAt(lastSeen: number | null, chapter: number): boolean {
  return lastSeen !== null && lastSeen < chapter;
}

export interface Visibility {
  chapter: number;
  /** 可见人物 id */
  characters: Set<string>;
  /** 可见关系(用 relations 数组下标标识, 与元素 id `rel-<index>` 对应) */
  relations: Set<number>;
  /** 可见势力 id(至少有一个可见成员) */
  factions: Set<string>;
}

/**
 * @param chapter 当前读到的章节(1..totalChapters)
 */
export function computeVisibility(data: GraphDemoData, chapter: number): Visibility {
  const characters = new Set<string>();
  for (const character of data.characters) {
    if (character.firstSeen <= chapter) {
      characters.add(character.id);
    }
  }

  const relations = new Set<number>();
  data.relations.forEach((relation, index) => {
    if (!isRelationVisibleAt(relation, chapter)) return;
    // 两端人物都不在场上时这条关系没有意义
    if (!characters.has(relation.from) || !characters.has(relation.to)) return;
    relations.add(index);
  });

  const factions = new Set<string>();
  for (const faction of data.factions) {
    const members = faction.members.length > 0
      ? faction.members
      : data.characters.filter((character) => character.faction === faction.id).map((character) => character.id);
    if (members.some((member) => characters.has(member))) {
      factions.add(faction.id);
    }
  }

  return { chapter, characters, relations, factions };
}

/**
 * 这一章「身边还连着亲缘边」的人 —— 家族树里真正会被画出来的点.
 *
 * 家族树按可见亲缘边取人(见 graphElements.applyModeFilter): 亲缘边过期后节点自己还
 * visible, 却没人跟它连边, 布局只会把它堆在 (0,0). 所以「这一章是谁」必须和
 * 「这一章画谁」用同一份可见关系算, 面板人数才不会和画面打架.
 */
export function kinLinkedIds(data: GraphDemoData, visibility: Visibility): Set<string> {
  const linked = new Set<string>();
  for (const index of visibility.relations) {
    const relation = data.relations[index];
    if (relation === undefined || !isKinRelation(relation.type)) continue;
    if (!visibility.characters.has(relation.from) || !visibility.characters.has(relation.to)) continue;
    linked.add(relation.from);
    linked.add(relation.to);
  }
  return linked;
}

/**
 * 可见亲缘子图的连通分量(一家人一块), 按人数降序、同人数按 id 排序.
 *
 * 与 kinLinkedIds 同一口径: 只连两端都可见的亲缘边, 没有任何可见亲缘边的人不进结果.
 * 页面用它算「这一章分成几家」, 单测用它核对 1749 章下左中两簇是否重新连在一起.
 */
export function kinComponents(data: GraphDemoData, visibility: Visibility): string[][] {
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

  const linked = kinLinkedIds(data, visibility);
  for (const id of linked) parent.set(id, id);
  for (const index of visibility.relations) {
    const relation = data.relations[index];
    if (relation === undefined || !isKinRelation(relation.type) || relation.from === relation.to) continue;
    if (!linked.has(relation.from) || !linked.has(relation.to)) continue;
    const rootA = find(relation.from);
    const rootB = find(relation.to);
    if (rootA !== rootB) parent.set(rootA, rootB);
  }

  const groups = new Map<string, string[]>();
  for (const id of linked) {
    const root = find(id);
    const bucket = groups.get(root) ?? [];
    bucket.push(id);
    groups.set(root, bucket);
  }
  return [...groups.values()]
    .map((ids) => [...ids].sort())
    .sort((a, b) => b.length - a.length || (a[0] ?? "").localeCompare(b[0] ?? ""));
}
