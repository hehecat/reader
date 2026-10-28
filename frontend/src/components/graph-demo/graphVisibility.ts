/**
 * 时间维度: 「读到第 N 章」时哪些人物/关系/势力还在场上.
 *
 * 纯函数, 方便页面在拖动滑块时只做一次计算, 再把结果翻成 cytoscape class.
 */
import type { GraphDemoData } from "@/types/graphDemo";

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
    const started = relation.from_chapter <= chapter;
    // to_chapter 为 null 表示「至今仍在」
    const notEnded = relation.to_chapter === null || relation.to_chapter >= chapter;
    if (!started || !notEnded) return;
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
