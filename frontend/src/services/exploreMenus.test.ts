/**
 * `parseExploreMenus` 的单测: 书海分类栏的数据源.
 *
 * 缺陷背景(2026-10-30 用户报「书海无法正常使用」): 「书客吧」的 exploreUrl 是 113 行
 * 美化过的 JSON, 末尾多一个逗号 → JSON.parse 抛错 → 旧代码退回「每行一个分类」的文本解析,
 * 于是整段 JSON 的每一行都变成一个分类按钮, 书海分类栏被 `{ "title": "玄幻魔法", ...` 铺满.
 */
import assert from "node:assert/strict";
import test from "node:test";

const menus = await import("./exploreMenus.ts");

test("尾逗号的 JSON 数组仍能解析出分类(书客吧的真实 exploreUrl 形状)", () => {
  const raw = `[
  { "title": "❀分类❀", "url": "/list_f_f_f_f_f_{{page}}.html", "style": { "layout_flexGrow": 1, }, },
  { "title": "玄幻魔法", "url": "/list_1_f_f_f_f_{{page}}.html", },
]`;
  assert.deepEqual(menus.parseExploreMenus({ exploreUrl: raw }), [
    { name: "❀分类❀", url: "/list_f_f_f_f_f_{{page}}.html" },
    { name: "玄幻魔法", url: "/list_1_f_f_f_f_{{page}}.html" },
  ]);
});

test("坏到无法解析的 JSON 不许退化成文本解析(否则一行一个假分类)", () => {
  const raw = `[
  { "title": "玄幻魔法", "url": "/list_1_f_f_f_f_{page}.html",
  { "title": "都市言情" ]]] ;;;`;
  assert.deepEqual(menus.parseExploreMenus({ exploreUrl: raw }), []);
});

test("合法 JSON 数组照旧解析, name 字段兜底 title", () => {
  const raw = '[{"name":"武侠","url":"/wuxia/{{page}}.html"},{"title":"科幻","url":"/kehuan/{{page}}.html","style":{"x":1}}]';
  assert.deepEqual(menus.parseExploreMenus({ exploreUrl: raw }), [
    { name: "武侠", url: "/wuxia/{{page}}.html" },
    { name: "科幻", url: "/kehuan/{{page}}.html" },
  ]);
});

test("文本形式的 exploreUrl: 每行 名称::地址, 空行拉平", () => {
  const raw = "玄幻::/xuanhuan/{{page}}.html\n\n历史::/lishi/{{page}}.html\n";
  assert.deepEqual(menus.parseExploreMenus({ exploreUrl: raw }), [
    { name: "玄幻", url: "/xuanhuan/{{page}}.html" },
    { name: "历史", url: "/lishi/{{page}}.html" },
  ]);
});

test("动态脚本形式不进书海", () => {
  assert.deepEqual(menus.parseExploreMenus({ exploreUrl: "@js: result" }), []);
  assert.deepEqual(menus.parseExploreMenus({ exploreUrl: "" }), []);
});
