/**
 * searchQuiet 单测 (node --test, 无第三方依赖).
 *
 * 覆盖静默态状态机的关键不变量:
 * - 纯游标推进(delta 0)/简介补全不打断静默;
 * - 静默中真有新结果 → 立刻回到正常态并计数(不静默插入);
 * - 同一波新结果累加, 提示到期只收提示不动静默;
 * - 准入 canSettle 在「未搜索 / 有错 / 0 条」时一律 false(不误记成功、不谎报有结果);
 * - reset 之后与初始态等价(结束/停止/错误都能退出静默)。
 */
import assert from "node:assert/strict";
import test from "node:test";

import type { QuietEvent, QuietState } from "./searchQuiet.ts";

const { QUIET_AFTER_MS, QUIET_INITIAL, QUIET_NOTICE_MS, canSettle, reduceQuiet } = await import(
  "./searchQuiet.ts"
);

/** 把一串事件依次灌进状态机 */
function run(events: ReadonlyArray<QuietEvent>, from: QuietState = QUIET_INITIAL): QuietState {
  return events.reduce((state, event) => reduceQuiet(state, event), from);
}

test("阈值常量落在 800~1500ms / 提示 1~5s 的约定区间", () => {
  assert.ok(QUIET_AFTER_MS >= 800 && QUIET_AFTER_MS <= 1_500, "静默阈值");
  assert.ok(QUIET_NOTICE_MS >= 1_000 && QUIET_NOTICE_MS <= 5_000, "新到提示时长");
});

test("正常流式增长不弹「新到」提示: 首批结果只在状态行里涨计数", () => {
  let state: QuietState = QUIET_INITIAL;
  for (const delta of [1, 5, 39, 55]) {
    state = reduceQuiet(state, { type: "grow", delta });
  }
  assert.deepEqual(state, QUIET_INITIAL);
  // 无变化时返回同一对象, 调用方据此跳过 setState(每批结果都少一次渲染)
  assert.equal(reduceQuiet(state, { type: "grow", delta: 3 }), state);
});

test("静默中到达新结果: 立刻退出静默并亮出新到条数", () => {
  const seeded = reduceQuiet(QUIET_INITIAL, { type: "grow", delta: 100 });
  const settled = reduceQuiet(seeded, { type: "settle" });
  assert.deepEqual(settled, { quiet: true, arrived: 0 });

  const resumed = reduceQuiet(settled, { type: "grow", delta: 2 });
  assert.deepEqual(resumed, { quiet: false, arrived: 2 });

  // 同一波继续来: 累加而不是覆盖
  const more = reduceQuiet(resumed, { type: "grow", delta: 3 });
  assert.deepEqual(more, { quiet: false, arrived: 5 });

  // 提示到期只收提示, 不影响正常态
  assert.deepEqual(reduceQuiet(more, { type: "notice-clear" }), { quiet: false, arrived: 0 });
});

test("纯游标推进 / 简介补全(不新增结果)不会把静默态踢掉", () => {
  const settled = reduceQuiet({ quiet: false, arrived: 0 }, { type: "settle" });
  assert.deepEqual(settled, { quiet: true, arrived: 0 });
  assert.equal(reduceQuiet(settled, { type: "grow", delta: 0 }), settled);
  // 重复 settle 是幂等的(收尾期多次重连会反复判定)
  assert.equal(reduceQuiet(settled, { type: "settle" }), settled);
});

test("收尾期可以反复进出静默: settle → grow → settle", () => {
  const state = run([
    { type: "grow", delta: 100 },
    { type: "settle" },
    { type: "grow", delta: 1 },
    { type: "notice-clear" },
    { type: "settle" },
  ]);
  assert.deepEqual(state, { quiet: true, arrived: 0 });
});

test("reset 把任何态打回初始态(结束 / 停止 / 错误都能退出静默)", () => {
  for (const state of [
    { quiet: true, arrived: 0 },
    { quiet: false, arrived: 4 },
    { quiet: true, arrived: 9 },
  ]) {
    assert.deepEqual(reduceQuiet(state, { type: "reset" }), QUIET_INITIAL);
  }
  // 初始态再 reset 不产生新对象
  assert.equal(reduceQuiet(QUIET_INITIAL, { type: "reset" }), QUIET_INITIAL);
});

test("准入 canSettle: 未搜索 / 有错 / 0 条 一律不进静默", () => {
  assert.equal(canSettle({ searching: true, errored: false, count: 100 }), true);
  assert.equal(canSettle({ searching: false, errored: false, count: 100 }), false);
  assert.equal(canSettle({ searching: true, errored: true, count: 100 }), false);
  assert.equal(canSettle({ searching: true, errored: false, count: 0 }), false);
});

test("状态机是纯函数: 不改入参、不共享引用", () => {
  const frozen = Object.freeze({ quiet: true, arrived: 0 });
  const next = reduceQuiet(frozen, { type: "grow", delta: 7 });
  assert.deepEqual(next, { quiet: false, arrived: 7 });
  assert.notEqual(next, frozen);
  assert.deepEqual(frozen, { quiet: true, arrived: 0 });
  assert.deepEqual(QUIET_INITIAL, { quiet: false, arrived: 0 });
});
