import assert from "node:assert/strict";
import test from "node:test";

const { mergeOriginLatest } = await import("./originLatest.ts");

test("mergeOriginLatest: 各源各记各的最新章, 不互相覆盖", () => {
  // 实测: 搜「一世之尊」时同一本书在 9 个源上, 最新章各不相同
  let map = mergeOriginLatest(undefined, "srcA", "第1070章 劝阻~");
  map = mergeOriginLatest(map, "srcB", "已完结");
  map = mergeOriginLatest(map, "srcC", "番外：昨日星辰昨日风");
  assert.deepEqual(map, {
    srcA: "第1070章 劝阻~",
    srcB: "已完结",
    srcC: "番外：昨日星辰昨日风",
  });
});

test("mergeOriginLatest: 空标题不占位, 已有值不被抹掉", () => {
  const map = mergeOriginLatest({ srcA: "第10章 起" }, "srcA", "   ");
  assert.deepEqual(map, { srcA: "第10章 起" });
  const fresh = mergeOriginLatest(undefined, "srcB", undefined);
  assert.deepEqual(fresh, {}, "顶点小说这类源 search 结果不带最新章, 不该凭空造一个");
});

test("mergeOriginLatest: 合并后端带回的映射, 同源以本条命中为准", () => {
  const map = mergeOriginLatest({ srcA: "旧", srcB: "后端值" }, "srcA", "新");
  assert.deepEqual(map, { srcA: "新", srcB: "后端值" });
});
