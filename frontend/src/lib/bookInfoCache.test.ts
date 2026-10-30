/**
 * lib/bookInfoCache 里两组「纯逻辑」的单测 (node --test, 无第三方依赖, 不碰 DOM).
 *
 * 1. 查询键工厂: 详情弹窗 (BookDetailDialog) 与阅读页 (useBookData) 共用同一份 key.
 *    同书同源 ⇒ 同键 (进阅读页直接命中弹窗取到的详情/目录, 不再重抓);
 *    同书异源 / 异书 ⇒ 异键 (不许撞车, 换源后不许命中旧源数据).
 * 2. 首屏门控 resolveReaderBoot: 正文/目录/元信息任一到位就该进阅读框架 —
 *    这是「进书白屏」那个缺陷的判定核心 (以前只看元信息, 已就绪的正文要陪等).
 *
 * 与 chapterCache.test.ts 同风格: 动态 import 拿 .ts 源码, 交给 node 直接跑.
 */
import assert from "node:assert/strict";
import test from "node:test";

const cache = await import("./bookInfoCache.ts");

const BOOK_URL = "https://source-a.example/book/42";
/** 同名书的另一个源: URL 不同 (各源自己的详情页地址) */
const BOOK_URL_SAME_NAME = "https://source-b.example/book/42";
const ORIGIN_A = "https://source-a.example";
const ORIGIN_B = "https://source-b.example";

// ---------------------------------------------------------------------------
// 查询键工厂
// ---------------------------------------------------------------------------

test("同书同源 ⇒ 同一个键 (详情弹窗与阅读页共用一份缓存)", () => {
  // 两处各自调一次工厂, 模拟「弹窗取过 → 进阅读页再取」
  assert.deepEqual(cache.bookInfoQueryKey(BOOK_URL, ORIGIN_A), cache.bookInfoQueryKey(BOOK_URL, ORIGIN_A));
  assert.deepEqual(
    cache.chaptersQueryKey(BOOK_URL, ORIGIN_A),
    cache.chaptersQueryKey(BOOK_URL, ORIGIN_A),
  );
  // 阅读页传的是 bookSourceUrl (本地书/源未知时为 undefined), 弹窗传 sourceOf(...) 后的同值
  assert.deepEqual(
    cache.chaptersQueryKey(BOOK_URL, undefined),
    cache.chaptersQueryKey(BOOK_URL, cache.UNKNOWN_SOURCE),
  );
});

test("同书异源 ⇒ 不同的键 (换源后不许命中旧源的详情/目录)", () => {
  assert.notDeepEqual(
    cache.bookInfoQueryKey(BOOK_URL, ORIGIN_A),
    cache.bookInfoQueryKey(BOOK_URL, ORIGIN_B),
  );
  assert.notDeepEqual(
    cache.chaptersQueryKey(BOOK_URL, ORIGIN_A),
    cache.chaptersQueryKey(BOOK_URL, ORIGIN_B),
  );
  // 已知源 vs 源未知也不能共键: 未知源那次抓到的可能是服务端默认源的目录
  assert.notDeepEqual(
    cache.chaptersQueryKey(BOOK_URL, undefined),
    cache.chaptersQueryKey(BOOK_URL, ORIGIN_A),
  );
});

test("同名书不同源 (bookUrl 不同) ⇒ 不同的键", () => {
  assert.notDeepEqual(
    cache.chaptersQueryKey(BOOK_URL, ORIGIN_A),
    cache.chaptersQueryKey(BOOK_URL_SAME_NAME, ORIGIN_B),
  );
  assert.notDeepEqual(
    cache.bookInfoQueryKey(BOOK_URL, ORIGIN_A),
    cache.bookInfoQueryKey(BOOK_URL_SAME_NAME, ORIGIN_A),
  );
});

test("源归一化: undefined / 空串 / 纯空白 是同一个「源未知」段", () => {
  assert.equal(cache.normalizeSource(undefined), cache.UNKNOWN_SOURCE);
  assert.equal(cache.normalizeSource(""), cache.UNKNOWN_SOURCE);
  assert.equal(cache.normalizeSource("   "), cache.UNKNOWN_SOURCE);
  assert.deepEqual(
    cache.chaptersQueryKey(BOOK_URL, "   "),
    cache.chaptersQueryKey(BOOK_URL, undefined),
  );
  assert.equal(cache.normalizeSource(` ${ORIGIN_A} `), ORIGIN_A);
});

test("bookInfo 与 chapters 不共用键 (命名空间隔离)", () => {
  assert.notDeepEqual(
    cache.bookInfoQueryKey(BOOK_URL, ORIGIN_A),
    cache.chaptersQueryKey(BOOK_URL, ORIGIN_A),
  );
});

test("键段数恒定, 前缀键是真前缀 (invalidateQueries 局部匹配 + 旧 2 段键镜像都依赖它)", () => {
  const key = cache.chaptersQueryKey(BOOK_URL, ORIGIN_A);
  const prefix = cache.chaptersQueryPrefix(BOOK_URL);
  // 源未知时也不能省略第 3 段, 否则前缀匹配会错位
  assert.equal(key.length, 3);
  assert.equal(cache.chaptersQueryKey(BOOK_URL, undefined).length, 3);
  assert.equal(prefix.length, 2);
  assert.deepEqual(key.slice(0, 2), prefix);
  // 前缀覆盖同一本书的所有源 (换源自愈就是靠这个失效)
  assert.deepEqual(cache.chaptersQueryKey(BOOK_URL, ORIGIN_B).slice(0, 2), prefix);
  // 但不会串到别的书
  assert.notDeepEqual(cache.chaptersQueryKey(BOOK_URL_SAME_NAME, ORIGIN_A).slice(0, 2), prefix);
  assert.deepEqual(cache.bookInfoQueryKey(BOOK_URL, ORIGIN_A).slice(0, 2), cache.bookInfoQueryPrefix(BOOK_URL));
});

test("本地书的 LOCAL_ORIGIN 由调用方映射, 工厂不做特判", () => {
  // 工厂刻意不 import LOCAL_ORIGIN (否则本模块就有了 hooks/ 的运行时依赖, node 跑不动);
  // 约定是详情弹窗的 sourceOf() 先把 "loc_book" 换成 undefined 再传 —— 与阅读页的
  // bookSourceUrl (本地书为 undefined) 落到同一段。
  const LOCAL_ORIGIN = "loc_book";
  assert.notDeepEqual(
    cache.chaptersQueryKey(BOOK_URL, LOCAL_ORIGIN),
    cache.chaptersQueryKey(BOOK_URL, undefined),
  );
});

// ---------------------------------------------------------------------------
// 阅读页首屏门控
// ---------------------------------------------------------------------------

test("门控: 正文就绪 + 元信息缺失 ⇒ 渲染正文 (不再整页挡)", () => {
  const decision = cache.resolveReaderBoot({
    hasBookUrl: true,
    bookReady: false,
    chapterCount: 120,
    bodyReady: true,
  });
  assert.equal(decision.phase, "reading");
  assert.equal(decision.metaDegraded, true, "元信息缺失要标记出来, 好走标题占位等降级");
});

test("门控: 目录先从 IndexedDB 缓存到、正文还在取 ⇒ 也进框架", () => {
  const decision = cache.resolveReaderBoot({
    hasBookUrl: true,
    bookReady: false,
    chapterCount: 120,
    bodyReady: false,
  });
  assert.equal(decision.phase, "reading");
  assert.equal(decision.metaDegraded, true);
});

test("门控: 元信息先到、正文未到 ⇒ 进框架, 正文由 ContentView 呈现 pending", () => {
  const decision = cache.resolveReaderBoot({
    hasBookUrl: true,
    bookReady: true,
    chapterCount: 0,
    bodyReady: false,
  });
  assert.equal(decision.phase, "reading");
  assert.equal(decision.metaDegraded, false);
});

test("门控: 正文取新失败 (isError) 且元信息缺失 ⇒ 仍进框架, 由正文区报错", () => {
  const decision = cache.resolveReaderBoot({
    hasBookUrl: true,
    bookReady: false,
    chapterCount: 3,
    bodyReady: true,
  });
  assert.equal(decision.phase, "reading");
});

test("门控: 三样都没到 ⇒ 才整页 loading/错误态", () => {
  const decision = cache.resolveReaderBoot({
    hasBookUrl: true,
    bookReady: false,
    chapterCount: 0,
    bodyReady: false,
  });
  assert.equal(decision.phase, "blocked");
  assert.equal(decision.metaDegraded, false);
});

test("门控: 缺 bookUrl ⇒ missing-url (优先于其他任何输入)", () => {
  assert.equal(
    cache.resolveReaderBoot({
      hasBookUrl: false,
      bookReady: true,
      chapterCount: 5,
      bodyReady: true,
    }).phase,
    "missing-url",
  );
  assert.equal(
    cache.resolveReaderBoot({ hasBookUrl: false, bookReady: false, chapterCount: 0, bodyReady: false }).phase,
    "missing-url",
  );
});

test("门控是纯函数: 同输入同输出, 且不改输入对象", () => {
  const input = { hasBookUrl: true, bookReady: false, chapterCount: 7, bodyReady: false };
  const snapshot = JSON.stringify(input);
  assert.deepEqual(cache.resolveReaderBoot(input), cache.resolveReaderBoot(input));
  assert.equal(JSON.stringify(input), snapshot);
});
