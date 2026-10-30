/**
 * graphTreeLayout 单测 (node --test, 无第三方依赖).
 *
 * 数据是 __fixtures__/kin-1749.json: 生产快照(16:05 重生成的那份)按第 1749 章可见性
 * (graphVisibility.computeVisibility / isRelationVisibleAt 的口径)裁出来的亲缘子图,
 * 71 人 / 83 条 parent+spouse+sibling 边, 分成 10 个互不相连的块, 最大块 45 人.
 * 人数随快照变, 所以块数/人数一律现算, 不写死. 没有增删任何人物或关系 ——
 * 数据上是孤岛的人, 这里也让它单独立块.
 *
 * 守的是「像谱系树」这件事本身:
 *   1. 代数严格成行, parent 边子代一定在父代下面一行;
 *   2. 任意两点不重叠(同层至少隔一格);
 *   3. 「跨 ≥2 代且 >2 格」的 parent 边只剩一条已知例外(田芸→李玄锋, 手足+跨代婚姻);
 *   4. 夫妻并排紧邻, 同 y;
 *   5. 同一块里不同分支的横向区间互不相交(不会交织成长斜线);
 *   6. 同输入两次结果逐位相同;
 *   7. 布局耗时远低于一帧预算.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  KIN_CELL,
  KIN_DETACHED_LABEL,
  KIN_ROW_GAP,
  KIN_RULER_GUTTER,
  computeKinTreeLayout,
} from "./graphTreeLayout.ts";

interface Fixture {
  meta: {
    chapter: number;
    charactersInSnapshot: number;
    relationsInSnapshot: number;
    visibleCharacters: number;
    visibleRelations: number;
    kinRelations: number;
    kinCharacters: number;
  };
  names: Record<string, string>;
  nodes: { id: string }[];
  edges: { source: string; target: string; type: string }[];
}

const fixture = JSON.parse(
  readFileSync(new URL("./__fixtures__/kin-1749.json", import.meta.url), "utf8"),
) as Fixture;

const NAME = (id: string): string => fixture.names[id] ?? id;
const label = (id: string): string => `${NAME(id)}(${id})`;

const layout = computeKinTreeLayout(fixture.nodes, fixture.edges);
const positions = layout.positions;
const blockOf = new Map<string, number>();
layout.blocks.forEach((block, index) => block.ids.forEach((id) => blockOf.set(id, index)));

interface Point {
  x: number;
  y: number;
}

/** 非空数组取首个(tsconfig 开了 noUncheckedIndexedAccess, 这里把「一定存在」显式表达出来) */
function firstOf<T>(list: readonly T[]): T {
  const value = list[0];
  if (value === undefined) throw new Error("空数组取首个");
  return value;
}

function at<T>(list: readonly T[], index: number): T {
  const value = list[index];
  if (value === undefined) throw new Error(`数组越界: ${index}`);
  return value;
}

function point(id: string): Point {
  const found = positions.get(id);
  assert.ok(found, `节点 ${label(id)} 没有被摆到位置`);
  return found;
}

function kinEdges(type: string): { source: string; target: string; type: string }[] {
  return fixture.edges.filter((edge) => edge.type === type);
}

/** 夫妻并成「单元」: 布局里横向紧邻的一对(间距一格)就是同一个单元 */
function spouseUnits(): Map<string, string[]> {
  const parent = new Map<string, string>();
  const find = (id: string): string => {
    let root = parent.get(id) ?? id;
    if (root !== id) {
      root = find(root);
      parent.set(id, root);
    }
    return root;
  };
  for (const edge of kinEdges("spouse")) {
    const a = find(edge.source);
    const b = find(edge.target);
    if (a !== b) parent.set(a, b);
  }
  const units = new Map<string, string[]>();
  for (const node of fixture.nodes) {
    const root = find(node.id);
    const bucket = units.get(root);
    if (bucket) bucket.push(node.id);
    else units.set(root, [node.id]);
  }
  for (const members of units.values()) members.sort();
  return units;
}

const units = spouseUnits();
const unitOf = (id: string): string => {
  for (const [root, members] of units) if (members.includes(id)) return root;
  throw new Error(`节点 ${label(id)} 不在任何单元里`);
};

// ---------------------------------------------------------------------------
// BASELINE: 改版前的 layoutKinComponent(纯函数复刻, 常量与旧代码一致)
//
// 旧算法的问题(见 graphTreeLayout.ts 顶部注释): 每一代单独排序, cursorX 从 0 重新往右排,
// 层与层之间没有「带」的概念 → 父代子代的 x 各排各的, 出现跨 2~4 行的长斜线.
// 这里原样搬过来只为了在同一份数据上算「改前」的对照数字, 不参与任何实现.
// ---------------------------------------------------------------------------
const BASE_LAYER_GAP = 132;
const BASE_NODE_GAP = 128;

function baselineLayout(): { positions: Map<string, Point>; rows: Map<string, number> } {
  const KIN = new Set(["parent", "spouse", "sibling"]);
  const edges = fixture.edges.filter((edge) => KIN.has(edge.type));
  // 节点顺序照抄 fixture(即页面传给布局的 cytoscape 元素顺序): 旧算法对入参顺序敏感,
  // 顺序一变 |dx| 就跟着变, 对照数字必须在这个顺序下算.
  const linked = fixture.nodes.map((node) => node.id);
  const parentUf = new Map<string, string>();
  const find = (id: string): string => {
    let root = parentUf.get(id) ?? id;
    if (root !== id) {
      root = find(root);
      parentUf.set(id, root);
    }
    return root;
  };
  const union = (a: string, b: string): void => {
    const ra = find(a);
    const rb = find(b);
    if (ra !== rb) parentUf.set(ra, rb);
  };
  for (const id of linked) union(id, id);
  for (const edge of edges) union(edge.source, edge.target);

  const components = new Map<string, Set<string>>();
  for (const id of linked) {
    const root = find(id);
    const bucket = components.get(root);
    if (bucket) bucket.add(id);
    else components.set(root, new Set([id]));
  }

  const positions = new Map<string, Point>();
  const rows = new Map<string, number>();
  for (const members of components.values()) {
    const inBlock = [...members];
    const blockEdges = edges.filter((edge) => members.has(edge.source) && members.has(edge.target));
    const depth = new Map<string, number>();
    for (const id of inBlock) depth.set(id, 0);
    const parentOf = new Map<string, string[]>();
    for (const edge of blockEdges) {
      if (edge.type !== "parent") continue;
      const list = parentOf.get(edge.target);
      if (list) list.push(edge.source);
      else parentOf.set(edge.target, [edge.source]);
    }
    // 旧代码就是 40 轮松弛 + 封顶 12
    for (let iter = 0; iter < 40; iter += 1) {
      let changed = false;
      for (const edge of blockEdges) {
        const da = depth.get(edge.source) ?? 0;
        const db = depth.get(edge.target) ?? 0;
        if (edge.type === "parent") {
          const next = Math.min(da + 1, 12);
          if (db < next) {
            depth.set(edge.target, next);
            changed = true;
          }
        } else {
          const merged = Math.min(Math.max(da, db), 12);
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

    const clusterUf = new Map<string, string>();
    const clusterFind = (id: string): string => {
      let root = clusterUf.get(id) ?? id;
      if (root !== id) {
        root = clusterFind(root);
        clusterUf.set(id, root);
      }
      return root;
    };
    for (const edge of blockEdges) {
      if (edge.type === "parent") continue;
      const ra = clusterFind(edge.source);
      const rb = clusterFind(edge.target);
      if (ra !== rb) clusterUf.set(ra, rb);
    }

    const layers = new Map<number, string[]>();
    for (const id of inBlock) {
      const layer = depth.get(id) ?? 0;
      const list = layers.get(layer);
      if (list) list.push(id);
      else layers.set(layer, [id]);
    }
    const minLayer = Math.min(...layers.keys());
    const placed = new Map<string, number>();
    for (const rawLayer of [...layers.keys()].sort((a, b) => a - b)) {
      const layer = rawLayer - minLayer;
      const clusters = new Map<string, string[]>();
      for (const id of layers.get(rawLayer) ?? []) {
        const key = clusterFind(id);
        const bucket = clusters.get(key);
        if (bucket) bucket.push(id);
        else clusters.set(key, [id]);
      }
      const ordered = [...clusters.values()].map((cluster) => {
        const xs: number[] = [];
        for (const member of cluster) {
          for (const parentId of parentOf.get(member) ?? []) {
            const x = placed.get(parentId);
            if (x !== undefined) xs.push(x);
          }
        }
        const anchor = xs.length > 0 ? xs.reduce((sum, x) => sum + x, 0) / xs.length : Number.MAX_SAFE_INTEGER;
        return { cluster, anchor };
      });
      ordered.sort((a, b) => a.anchor - b.anchor || firstOf(a.cluster).localeCompare(firstOf(b.cluster)));
      let cursorX = 0;
      const y = layer * BASE_LAYER_GAP;
      for (const item of ordered) {
        for (const id of item.cluster) {
          positions.set(id, { x: cursorX, y });
          placed.set(id, cursorX);
          rows.set(id, layer);
          cursorX += BASE_NODE_GAP;
        }
      }
    }
  }
  // 旧代码之后按货架平移整块(packKinBlocks), 只改块的绝对位置, 不影响块内 dx → 这里略过
  return { positions, rows };
}

const baseline = baselineLayout();

/** 「跨 ≥ 2 代, 水平还差 2 格以上」的 parent 边: 就是截图里那些长斜线 */
function longDiagonalEdges(
  points: Map<string, Point>,
  rows: Map<string, number>,
  edges: { source: string; target: string; type: string }[],
): string[] {
  const bad: string[] = [];
  for (const edge of edges) {
    const from = points.get(edge.source);
    const to = points.get(edge.target);
    if (!from || !to) continue;
    const span = Math.abs((rows.get(edge.target) ?? 0) - (rows.get(edge.source) ?? 0));
    const dx = Math.abs(to.x - from.x);
    if (span >= 2 && dx > 2 * KIN_CELL) {
      bad.push(`${NAME(edge.source)}→${NAME(edge.target)} 跨 ${span} 代, |dx|=${dx}`);
    }
  }
  return bad;
}

/**
 * 唯一豁免的长斜线。数据本身就做不到更短, 不是布局偷懒:
 * 田芸 与 田有道 是手足(要求同层), 而 田有道 的孩子 田荣 在第 1 代 → 这一行簇被钉在第 0 代,
 * 无法下沉; 但 田芸 的孩子 李玄锋 / 李景恬 在第 3 代, 于是 田芸→子 必然跨 3 代。
 * 李玄锋 另有第三个父代(李项平), 三个父代分布在 x=1380..4440, 主父只能认一个,
 * 剩下两条边天生很长。这是「多父 + 跨代婚姻」的数据现实, 布局消不掉。
 */
const DOCUMENTED_EXCEPTIONS = ["田芸→李玄锋"];

/** 亲缘连通块(与实现无关的复算, 只用来核对「布局没有增删人物/关系」) */
function fixtureComponents(): string[][] {
  const up = new Map<string, string>();
  for (const node of fixture.nodes) up.set(node.id, node.id);
  const find = (id: string): string => {
    const root = up.get(id) ?? id;
    if (root === id) return id;
    const resolved = find(root);
    up.set(id, resolved);
    return resolved;
  };
  for (const edge of fixture.edges) {
    const a = find(edge.source);
    const b = find(edge.target);
    if (a !== b) up.set(a, b);
  }
  const groups = new Map<string, string[]>();
  for (const node of fixture.nodes) {
    const root = find(node.id);
    const bucket = groups.get(root);
    if (bucket) bucket.push(node.id);
    else groups.set(root, [node.id]);
  }
  const cmp = (a: string, b: string): number => (a < b ? -1 : a > b ? 1 : 0);
  for (const group of groups.values()) group.sort(cmp);
  return [...groups.values()].sort((a, b) => b.length - a.length || cmp(firstOf(a), firstOf(b)));
}

/** parent 边 |dx| 的 p50 / p90 / max, 报告里的对照数字 */
function dxStats(
  points: Map<string, Point>,
  edges: { source: string; target: string; type: string }[],
): { p50: number; p90: number; max: number; count: number } {
  const values: number[] = [];
  for (const edge of edges) {
    const from = points.get(edge.source);
    const to = points.get(edge.target);
    if (!from || !to) continue;
    values.push(Math.abs(to.x - from.x));
  }
  values.sort((a, b) => a - b);
  const pick = (q: number): number => at(values, Math.min(values.length - 1, Math.floor(q * values.length)));
  return {
    p50: pick(0.5),
    p90: pick(0.9),
    max: values.length === 0 ? 0 : at(values, values.length - 1),
    count: values.length,
  };
}

/** 跨 ≥ 2 代的 parent 边(条数本身就是结构指标) */
function multiGenerationEdges(
  rows: Map<string, number>,
  edges: { source: string; target: string; type: string }[],
): string[] {
  const out: string[] = [];
  for (const edge of edges) {
    const span = (rows.get(edge.target) ?? 0) - (rows.get(edge.source) ?? 0);
    if (span >= 2) out.push(`${NAME(edge.source)}→${NAME(edge.target)} 跨 ${span} 代`);
  }
  return out;
}

const parentEdges = kinEdges("parent");
const baselineRows = new Map<string, number>();
for (const [id, row] of baseline.rows) baselineRows.set(id, row);

test("数据: 块数/最大块人数从 fixture 现算, 布局不增删人物也不造边", () => {
  assert.equal(fixture.meta.chapter, 1749);
  assert.equal(fixture.edges.length, fixture.meta.kinRelations);
  assert.equal(fixture.nodes.length, fixture.meta.kinCharacters);
  // 块 = 亲缘图的连通分量: 现算, 不写死人数
  const expected = fixtureComponents();
  assert.ok(expected.length > 1, "亲缘子图应该不止一块");
  assert.deepEqual(
    layout.blocks.map((block) => block.ids.length),
    expected.map((group) => group.length),
    "块的人数序列与亲缘连通分量对不上",
  );
  // 每个块的成员集合必须正好是某个连通分量: 不增人、不少人、不把人挪到别的块
  for (const block of layout.blocks) {
    const ids = new Set(block.ids);
    const match = expected.find((group) => group.length === ids.size && group.every((id) => ids.has(id)));
    assert.ok(match, `块 [${block.ids.map(NAME).join(", ")}] 不是一个完整的亲缘连通块`);
  }
  // 边也一条不多一条不少: 每条亲缘边都必须落在某个块内
  const inBlock = (ids: Set<string>): number =>
    fixture.edges.filter((edge) => ids.has(edge.source) && ids.has(edge.target)).length;
  let keptEdges = 0;
  for (const block of layout.blocks) keptEdges += inBlock(new Set(block.ids));
  assert.equal(keptEdges, fixture.edges.length, "布局把亲缘边丢在块外了");
  // 每个人都被摆到了位置, 且只出现在一个块里
  assert.equal(positions.size, fixture.nodes.length);
  const seen = new Set<string>();
  for (const block of layout.blocks) {
    for (const id of block.ids) {
      assert.ok(!seen.has(id), `${label(id)} 出现在多个块里`);
      seen.add(id);
    }
  }
  // 人数最多的主家排最前(左上)
  const largest = Math.max(...layout.blocks.map((block) => block.ids.length));
  assert.equal(firstOf(layout.blocks).ids.length, largest, "主家(人数最多)没排在最前");
  assert.equal(firstOf(layout.blocks).y, 0);
  // 主家贴最左, 只让开一条留给「第 N 代」标尺的沟
  assert.equal(firstOf(layout.blocks).x, KIN_RULER_GUTTER);
  assert.equal(firstOf(layout.blocks).x, Math.min(...layout.blocks.map((block) => block.x)));

  // 标尺只给主块: 「第 N 代」条数 == 主块代数跨度, 其它块各自只标一次「独立家系」
  const generations = layout.rows.filter((row) => row.text !== KIN_DETACHED_LABEL);
  assert.deepEqual(
    generations.map((row) => row.text),
    Array.from({ length: firstOf(layout.blocks).rows }, (_, index) => `第 ${index + 1} 代`),
    "主块的「第 N 代」标尺不全",
  );
  const detached = layout.rows.filter((row) => row.text === KIN_DETACHED_LABEL);
  assert.equal(detached.length, layout.blocks.length - 1, "独立家系标签数应等于非主块数量");
  assert.equal(layout.rows.length, generations.length + detached.length);
});

test("1. 代数严格成行: 同代数 y 完全相同, parent 边子代 y 严格大于父代", () => {
  const rowsByPosition = new Map<string, string[]>();
  for (const node of fixture.nodes) {
    const p = point(node.id);
    const key = `${blockOf.get(node.id)}@${layout.generation.get(node.id)}`;
    const bucket = rowsByPosition.get(key);
    if (bucket) bucket.push(`${p.y}`);
    else rowsByPosition.set(key, [`${p.y}`]);
  }
  for (const [key, ys] of rowsByPosition) {
    assert.equal(new Set(ys).size, 1, `${key} 这一代出现了多个 y: ${[...new Set(ys)].join(", ")}`);
  }
  // 每个块的 y 都落在自己那块的行网格上(块可以整体摆在第二个货架上, 但块内不会有人被摆到两行之间)
  for (const [index, block] of layout.blocks.entries()) {
    for (const id of block.ids) {
      const { y } = point(id);
      const row = (y - block.y) / KIN_ROW_GAP;
      assert.ok(
        Math.abs(row - Math.round(row)) < 1e-9,
        `块 ${index} 的 ${label(id)} y=${y} 不在行网格上(块顶 ${block.y})`,
      );
      assert.ok(row >= 0 && row < block.rows, `块 ${index} 的 ${label(id)} 落到了 block.rows 之外`);
    }
  }
  // 每个块占的行数 = 块内代数跨度 + 1
  for (const [index, block] of layout.blocks.entries()) {
    const gens = block.ids.map((id) => layout.generation.get(id) ?? 0);
    const span = Math.max(...gens) - Math.min(...gens) + 1;
    assert.equal(block.rows, span, `块 ${index} 的 rows 与代数跨度对不上`);
  }
  // 行距 ≥ 150
  assert.ok(KIN_ROW_GAP >= 150);
  for (const edge of parentEdges) {
    const from = point(edge.source);
    const to = point(edge.target);
    assert.ok(
      to.y > from.y,
      `parent 边 ${NAME(edge.source)}→${NAME(edge.target)} 的子代没有排在下面 (父 y=${from.y}, 子 y=${to.y})`,
    );
  }
  // 夫妻/手足拉平到同一层
  for (const edge of fixture.edges) {
    if (edge.type === "parent") continue;
    const from = point(edge.source);
    const to = point(edge.target);
    assert.equal(from.y, to.y, `${edge.type} 边 ${NAME(edge.source)}—${NAME(edge.target)} 不在同一行`);
  }
});

test("2. 任意两点不重叠: 同层不同单元 |dx| ≥ 1 格, 块之间也不重叠", () => {
  // 格距有个硬地板: 「黎泾李家」57 人实测 fit 后 34% × 140 = 48px/格, 4 字姓名在 11px 字下占 44px,
  // 还留 4px 白; 再窄(≤120)名字就连字了. 这是可读性下限, 不是审美偏好.
  assert.ok(KIN_CELL >= 140, `格距 ${KIN_CELL} 太窄, 4 字姓名会连字`);
  const all = fixture.nodes.map((node) => ({ id: node.id, ...point(node.id) }));
  for (let i = 0; i < all.length; i += 1) {
    for (let j = i + 1; j < all.length; j += 1) {
      const a = at(all, i);
      const b = at(all, j);
      if (Math.abs(a.y - b.y) > 1e-9) continue;
      const dx = Math.abs(a.x - b.x);
      if (dx < 1e-9) assert.fail(`${label(a.id)} 与 ${label(b.id)} 完全重合`);
      const sameUnit = unitOf(a.id) === unitOf(b.id);
      if (sameUnit) {
        assert.ok(dx >= KIN_CELL - 1e-9, `${label(a.id)} 与 ${label(b.id)} 是夫妻, 只隔了 ${dx}`);
      } else {
        assert.ok(dx >= KIN_CELL - 1e-9, `${label(a.id)} 与 ${label(b.id)} 同层只隔 ${dx}(< 1 格)`);
      }
    }
  }
  // 块与块的包围盒互不相交
  for (let i = 0; i < layout.blocks.length; i += 1) {
    for (let j = i + 1; j < layout.blocks.length; j += 1) {
      const a = at(layout.blocks, i);
      const b = at(layout.blocks, j);
      const overlapX = a.x < b.x + b.width && b.x < a.x + a.width;
      const overlapY = a.y < b.y + b.height && b.y < a.y + a.height;
      assert.ok(!(overlapX && overlapY), `块 ${i} 与块 ${j} 重叠了`);
    }
  }
});

test("3. 结构指标: 「跨 ≥2 代且 > 2 格」的 parent 边只剩已知例外, |dx| 分位数与跨代条数见日志", () => {
  // 主判据是结构性的: 长斜线会真的出现在截图上, 中位数降幅只是副产品(且容易被一两条极值带跑).
  const before = dxStats(baseline.positions, parentEdges);
  const after = dxStats(positions, parentEdges);
  const beforeBad = longDiagonalEdges(baseline.positions, baselineRows, parentEdges);
  const afterBad = longDiagonalEdges(positions, layout.generation, parentEdges);
  const beforeMulti = multiGenerationEdges(baselineRows, parentEdges);
  const afterMulti = multiGenerationEdges(layout.generation, parentEdges);

  assert.ok(beforeBad.length > afterBad.length, `改前的长斜线(${beforeBad.length})不比改后(${afterBad.length})多, 对照数据不对`);
  // 例外白名单必须真的出现: 既不能有清单外的长斜线, 也不许留下僵尸豁免
  const unexpected = afterBad.filter(
    (item) => !DOCUMENTED_EXCEPTIONS.some((name) => item.startsWith(`${name} `)),
  );
  assert.deepEqual(unexpected, [], `还有已知例外之外的长斜线: ${unexpected.join(" / ")}`);
  assert.equal(afterBad.length, DOCUMENTED_EXCEPTIONS.length, `已知例外条数与清单不一致: ${afterBad.join(" / ")}`);

  // parent 边本身不该跨代: 剩下的跨代边只能是例外里那一条链
  assert.ok(afterMulti.length <= afterBad.length + DOCUMENTED_EXCEPTIONS.length, `跨 ≥2 代的边太多: ${afterMulti.join(" / ")}`);
  assert.ok(afterMulti.length < beforeMulti.length, `跨代边没减少: 改前 ${beforeMulti.length} → 改后 ${afterMulti.length}`);

  console.log(
    `[baseline] parent 边 ${before.count} 条, |dx| p50 ${before.p50} / p90 ${before.p90} / max ${before.max}, 跨≥2代 ${beforeMulti.length} 条, 跨≥2代且>2格 ${beforeBad.length} 条`,
  );
  console.log(
    `[now]      parent 边 ${after.count} 条, |dx| p50 ${after.p50} / p90 ${after.p90} / max ${after.max}, 跨≥2代 ${afterMulti.length} 条, 跨≥2代且>2格 ${afterBad.length} 条(已知例外) → ${afterBad.join(" / ")}`,
  );
  console.log(`[例外] ${DOCUMENTED_EXCEPTIONS.join(" / ")}: 手足+跨代婚姻导致, 见 DOCUMENTED_EXCEPTIONS 注释`);
  // 如实记录 p50 的相对变化, 不做门槛断言
  console.log(`[p50] ${before.p50} → ${after.p50} (${(((after.p50 - before.p50) / before.p50) * 100).toFixed(1)}%)`);
});

test("4. 夫妻单元内 |dx| == 1 格 且 |dy| == 0", () => {
  const spouse = kinEdges("spouse");
  assert.ok(spouse.length > 0);
  for (const edge of spouse) {
    const from = point(edge.source);
    const to = point(edge.target);
    assert.equal(from.y, to.y, `${NAME(edge.source)}—${NAME(edge.target)} 不在同一行`);
    assert.equal(Math.abs(to.x - from.x), KIN_CELL, `${NAME(edge.source)}—${NAME(edge.target)} 不是紧邻一格`);
  }
  // 一夫多妻(李玄宣 有 木芽鹿/窦夫人)时单元会变 3 人: 仍然必须连成一串, 中间不能插外人
  for (const members of units.values()) {
    if (members.length < 2) continue;
    const xs = members.map((id) => point(id).x).sort((a, b) => a - b);
    for (let i = 1; i < xs.length; i += 1) {
      assert.equal(
        at(xs, i) - at(xs, i - 1),
        KIN_CELL,
        `夫妻单元 ${members.map(NAME).join("/")} 里有人被拆开`,
      );
    }
    assert.equal(new Set(members.map((id) => point(id).y)).size, 1);
  }
});

test("5. 同一块里任意两个不同分支的横向区间不相交", () => {
  // 分支 = 夫妻单元; 区间 = 该单元 + 它所有后代(按「主父」挂靠的树)的 x 范围.
  //
  // 「主父」是布局契约的一部分, 不是实现细节: 一个单元只能挂在一条树上(几何上一个点不能同时
  // 在两个父代下方居中), 双父的情况(李玄宣 有 李长湖/李通崖 两个父亲)挂在「子树更大」的那个
  // 父单元下, 平手时按 id 小的. 另一条 parent 边仍然保留(只是子代不会居中于它),
  // 这也是第 3 条里那条跨代偏移的来源 —— 但它跨 1 代, 不算长斜线.
  const parentsOfUnit = new Map<string, Set<string>>();
  const childrenOfUnit = new Map<string, Set<string>>();
  for (const root of units.keys()) {
    parentsOfUnit.set(root, new Set());
    childrenOfUnit.set(root, new Set());
  }
  for (const edge of parentEdges) {
    const from = unitOf(edge.source);
    const to = unitOf(edge.target);
    if (from === to) continue;
    parentsOfUnit.get(to)?.add(from);
    childrenOfUnit.get(from)?.add(to);
  }
  const primaryParentOf = (unit: string): string | null => {
    const candidates = [...(parentsOfUnit.get(unit) ?? [])].sort();
    let best: string | null = null;
    let bestCount = -1;
    for (const candidate of candidates) {
      const count = (childrenOfUnit.get(candidate) ?? new Set()).size;
      if (count > bestCount) {
        best = candidate;
        bestCount = count;
      }
    }
    return best;
  };
  const primaryChildren = new Map<string, Set<string>>();
  for (const root of units.keys()) primaryChildren.set(root, new Set());
  const roots: string[] = [];
  for (const root of [...units.keys()].sort()) {
    const primary = primaryParentOf(root);
    if (primary === null) roots.push(root);
    else primaryChildren.get(primary)?.add(root);
  }
  assert.ok(roots.length >= 2, "主家之外没有别的树了, 这个断言就失去意义");
  const subtreeOf = (root: string): Set<string> => {
    const out = new Set<string>();
    const stack = [root];
    while (stack.length > 0) {
      const key = stack.pop() as string;
      if (out.has(key)) continue;
      out.add(key);
      for (const kid of primaryChildren.get(key) ?? []) stack.push(kid);
    }
    return out;
  };
  const membersOfUnit = new Map<string, string[]>();
  for (const [root, members] of units) membersOfUnit.set(root, members);
  const spanOf = (roots: Set<string>): { left: number; right: number } => {
    let left = Number.POSITIVE_INFINITY;
    let right = Number.NEGATIVE_INFINITY;
    for (const root of roots) {
      for (const id of membersOfUnit.get(root) ?? []) {
        const { x } = point(id);
        left = Math.min(left, x);
        right = Math.max(right, x);
      }
    }
    return { left, right };
  };

  const unitIds = [...membersOfUnit.keys()].sort();
  const subtreeCache = new Map<string, Set<string>>();
  for (const root of unitIds) subtreeCache.set(root, subtreeOf(root));

  let compared = 0;
  for (let i = 0; i < unitIds.length; i += 1) {
    for (let j = i + 1; j < unitIds.length; j += 1) {
      const a = at(unitIds, i);
      const b = at(unitIds, j);
      const unitA = membersOfUnit.get(a) ?? [];
      const unitB = membersOfUnit.get(b) ?? [];
      const blockA = unitA.length > 0 ? blockOf.get(firstOf(unitA)) : undefined;
      const blockB = unitB.length > 0 ? blockOf.get(firstOf(unitB)) : undefined;
      if (blockA === undefined || blockA !== blockB) continue;
      const treeA = subtreeCache.get(a) as Set<string>;
      const treeB = subtreeCache.get(b) as Set<string>;
      // 祖孙关系不算「不同分支」: 祖先的区间本来就该罩住后代
      if (treeA.has(b) || treeB.has(a)) continue;
      const spanA = spanOf(treeA);
      const spanB = spanOf(treeB);
      compared += 1;
      const disjoint = spanA.right < spanB.left - 1e-9 || spanB.right < spanA.left - 1e-9;
      assert.ok(
        disjoint,
        `${NAME(unitA.length > 0 ? firstOf(unitA) : a)} 分支 [${spanA.left}, ${spanA.right}] 与 ` +
          `${NAME(unitB.length > 0 ? firstOf(unitB) : b)} 分支 [${spanB.left}, ${spanB.right}] 交织了`,
      );
    }
  }
  assert.ok(compared > 10, `只比较了 ${compared} 对分支, 覆盖不够`);
});

test("6. 确定性: 同输入两次运行, positions 逐位相同", () => {
  const again = computeKinTreeLayout(fixture.nodes, fixture.edges);
  assert.equal(again.positions.size, positions.size);
  for (const [id, p] of positions) {
    const q = again.positions.get(id);
    assert.ok(q, `${label(id)} 第二次没摆`);
    assert.ok(Object.is(q.x, p.x) && Object.is(q.y, p.y), `${label(id)} 两次坐标不同: ${JSON.stringify(p)} vs ${JSON.stringify(q)}`);
  }
  for (const [id, gen] of layout.generation) assert.equal(again.generation.get(id), gen);
  assert.deepEqual(again.blocks, layout.blocks);
  assert.deepEqual(again.rows, layout.rows);
  // 入参顺序打乱也不影响结果(排序键全部落到 id)
  const shuffledNodes = [...fixture.nodes].reverse();
  const shuffledEdges = [...fixture.edges].reverse();
  const fromShuffled = computeKinTreeLayout(shuffledNodes, shuffledEdges);
  for (const [id, p] of positions) {
    const q = fromShuffled.positions.get(id);
    assert.ok(q && Object.is(q.x, p.x) && Object.is(q.y, p.y), `${label(id)} 对入参顺序敏感`);
  }
});

test("7. 性能: 真实亲缘子图整块布局 < 30ms", () => {
  // 页面实际喂进来的是「当前可见的全部人物 + 可见亲缘边」(见 layoutFamilyTree),
  // 人数随快照变, 所以这里只断言量级, 具体人数打到日志里.
  const allNodes = [...fixture.nodes];
  assert.ok(allNodes.length >= 50);
  const run = (): number => {
    const started = process.hrtime.bigint();
    computeKinTreeLayout(allNodes, fixture.edges);
    const ended = process.hrtime.bigint();
    return Number(ended - started) / 1e6;
  };
  for (let i = 0; i < 3; i += 1) run(); // 预热 / 让 JIT 先把热路径编出来
  const samples = Array.from({ length: 7 }, run).sort((a, b) => a - b);
  const median = at(samples, Math.floor(samples.length / 2));
  console.log(`[perf] ${allNodes.length} 人 / ${fixture.edges.length} 边, 中位数 ${median.toFixed(2)}ms`);
  assert.ok(median < 30, `布局耗时 ${median.toFixed(2)}ms 超过 30ms`);
});
