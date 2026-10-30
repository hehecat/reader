/**
 * graphVisibility 单测 (node --test, 无第三方依赖 / 无 jsdom).
 *
 * 守的是「读到第 N 章谁还在场上」这条口径, 尤其是 1749 章家族树被打成三簇那个 bug:
 * 血缘边的 to_chapter 只是「最后一次被提到」(李木田 → 李长湖 [parent] 4..1711),
 * 不能当成失效章, 否则已故的老人被整块从世系里删掉, 族人被切成互不相连的几簇.
 */
import assert from "node:assert/strict";
import test from "node:test";

import type { GraphCharacter, GraphDemoData, GraphRelation } from "@/types/graphDemo";

import {
  computeVisibility,
  isDeceasedAt,
  isKinRelation,
  isRelationVisibleAt,
  kinComponents,
  kinLinkedIds,
} from "./graphVisibility.ts";

function character(id: string, overrides: Partial<GraphCharacter> = {}): GraphCharacter {
  return {
    id,
    name: overrides.name ?? id,
    aliases: [],
    gender: "未知",
    role: "",
    faction: null,
    generation: 1,
    firstSeen: 1,
    lastSeen: null,
    conflict: false,
    ...overrides,
  };
}

function relation(overrides: Partial<GraphRelation> & Pick<GraphRelation, "from" | "to" | "type">): GraphRelation {
  return {
    label: undefined,
    from_chapter: 1,
    to_chapter: null,
    quote: undefined,
    ...overrides,
  };
}

function dataOf(characters: GraphCharacter[], relations: GraphRelation[]): GraphDemoData {
  return {
    meta: {
      book: "测试书",
      totalChapters: 1749,
      sampledChapters: 0,
      generatedAt: "",
      schema: "graph-demo-v1",
    },
    characters,
    factions: [],
    memberships: [],
    relations,
    stats: { characters: characters.length, relations: relations.length, factions: 0 },
  } as GraphDemoData;
}

/** 李家: 李木田(祖父, 已故) 连着长湖(子)与项平(侄), 长湖-项平同父辈 → 一家人 */
function liFamily(): GraphDemoData {
  return dataOf(
    [
      character("c2", { name: "李木田", firstSeen: 2, lastSeen: 1711 }),
      character("c4", { name: "李长湖", firstSeen: 2, lastSeen: null }),
      character("c9", { name: "李项平", firstSeen: 2, lastSeen: null }),
    ],
    [
      // 抽取把「人物死亡/最后一次被提到」写进了血缘边的 to_chapter
      relation({ from: "c2", to: "c4", type: "parent", from_chapter: 4, to_chapter: 1711 }),
      relation({ from: "c2", to: "c9", type: "sibling", from_chapter: 2, to_chapter: 1711 }),
    ],
  );
}

test("血缘边跨过 to_chapter 仍然可见: parent 4..1711 在第 1749 章还成立", () => {
  const data = liFamily();
  const relationParent = data.relations[0]!;
  assert.equal(isRelationVisibleAt(relationParent, 1749), true);
  assert.equal(isRelationVisibleAt(relationParent, 1100), true);
  assert.equal(isRelationVisibleAt(relationParent, 3), false, "from_chapter 之前不可见");

  const visibility = computeVisibility(data, 1749);
  // 两条血缘边都还在 → 李木田不会因为「死在第 1711 章」被踢出世系
  assert.equal(visibility.relations.size, 2);
  assert.equal(visibility.characters.has("c2"), true);
  assert.deepEqual(kinComponents(data, visibility).map((block) => block.length), [3], "李木田把三簇连成一块");
});

test("sibling 同样不吃 to_chapter; 兄弟反目之类的 to_chapter 也不该拆开世系", () => {
  const data = liFamily();
  const relationSibling = data.relations[1]!;
  assert.equal(relationSibling.type, "sibling");
  assert.equal(isRelationVisibleAt(relationSibling, 1749), true);
  assert.equal(isRelationVisibleAt(relationSibling, 1), false, "from_chapter = 2 时第 1 章还没发生");
});

test("spouse 保持区间语义: to_chapter 过期后隐藏", () => {
  const spouse = relation({ from: "c1", to: "c2", type: "spouse", from_chapter: 183, to_chapter: 1680 });
  assert.equal(isRelationVisibleAt(spouse, 1679), true);
  assert.equal(isRelationVisibleAt(spouse, 1680), true, "末日当天还算在");
  assert.equal(isRelationVisibleAt(spouse, 1749), false, "第 1680 章之后夫妻关系已经结束");
  assert.equal(isRelationVisibleAt(spouse, 182), false, "from_chapter 之前不可见");

  // 同一对人: 只靠夫妻边连着 → 过期后不再是「一家人」(节点还在场上, 但没亲缘边)
  const data = dataOf(
    [character("c1", { name: "李妃若" }), character("c2", { name: "木焦蛮" })],
    [relation({ from: "c1", to: "c2", type: "spouse", from_chapter: 183, to_chapter: 1680 })],
  );
  const late = computeVisibility(data, 1749);
  assert.equal(late.characters.size, 2, "两个人都还在场上");
  assert.equal(late.relations.size, 0);
  assert.deepEqual(kinComponents(data, late), [], "没有可见亲缘边 → 谁都不进家族树");
  assert.equal(kinLinkedIds(data, late).size, 0);

  const early = computeVisibility(data, 1600);
  assert.equal(early.relations.size, 1);
  assert.deepEqual(kinComponents(data, early), [["c1", "c2"]]);
});

test("enemy / ally / master 等其他类型也继续吃 to_chapter", () => {
  const chapter = 1749;
  for (const type of ["enemy", "ally", "other", "master", "disciple"] as const) {
    assert.equal(
      isRelationVisibleAt(relation({ from: "c1", to: "c2", type, from_chapter: 10, to_chapter: 20 }), chapter),
      false,
      `${type} 过期后应当隐藏`,
    );
    assert.equal(
      isRelationVisibleAt(relation({ from: "c1", to: "c2", type, from_chapter: 10, to_chapter: null }), chapter),
      true,
      `${type} 至今仍在时应当可见`,
    );
  }
  // 只有亲子/手足是永久事实, 夫妻仍算亲缘(画进树)但有区间
  assert.equal(isKinRelation("parent"), true);
  assert.equal(isKinRelation("sibling"), true);
  assert.equal(isKinRelation("spouse"), true);
  assert.equal(isKinRelation("enemy"), false);
});

test("from_chapter > chapter 的血缘边仍然不可见(还没出生/还没相认)", () => {
  const data = dataOf(
    [character("a", { name: "甲" }), character("b", { name: "乙" })],
    [relation({ from: "a", to: "b", type: "parent", from_chapter: 300, to_chapter: 1711 })],
  );
  assert.equal(computeVisibility(data, 299).relations.size, 0);
  assert.equal(computeVisibility(data, 300).relations.size, 1);
  assert.deepEqual(kinComponents(data, computeVisibility(data, 299)), []);
});

test("已故人物仍出现在世系里: lastSeen 1711 的人在 1749 章保留节点, 只是标记已故", () => {
  const data = liFamily();
  const visibility = computeVisibility(data, 1749);
  assert.equal(isDeceasedAt(1711, 1749), true, "李木田已故");
  assert.equal(isDeceasedAt(null, 1749), false, "lastSeen 为 null = 全书都在场");
  assert.equal(isDeceasedAt(1749, 1749), false, "本章还在场不算已故");

  const blocks = kinComponents(data, visibility);
  assert.deepEqual(blocks, [["c2", "c4", "c9"]], "已故的李木田还在世系里, 且仍是连通节点");
  const linked = kinLinkedIds(data, visibility);
  assert.equal(linked.has("c2"), true);
});

test("端点还没出场的人物, 关系也不可见(血缘永久 ≠ 无视出场时间)", () => {
  const data = dataOf(
    [character("old", { name: "祖" }), character("young", { name: "孙", firstSeen: 500 })],
    [relation({ from: "old", to: "young", type: "parent", from_chapter: 1, to_chapter: 1200 })],
  );
  assert.equal(computeVisibility(data, 499).relations.size, 0, "孙还没出场");
  assert.equal(computeVisibility(data, 1749).relations.size, 1, "孙出场后血缘边一直在");
});

test("kinComponents 按人数降序且成员 id 稳定排序", () => {
  const data = dataOf(
    [
      character("x1", { name: "小甲" }),
      character("x2", { name: "小乙" }),
      character("y1", { name: "大甲" }),
      character("y2", { name: "大乙" }),
      character("y3", { name: "大丙" }),
    ],
    [
      relation({ from: "x2", to: "x1", type: "sibling" }),
      relation({ from: "y3", to: "y1", type: "parent" }),
      relation({ from: "y2", to: "y1", type: "sibling" }),
    ],
  );
  assert.deepEqual(kinComponents(data, computeVisibility(data, 1749)), [
    ["y1", "y2", "y3"],
    ["x1", "x2"],
  ]);
});
