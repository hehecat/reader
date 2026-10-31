import assert from "node:assert/strict";
import test from "node:test";

const { chapterTitleText, looksLikeChapterTitle, looksLikeSearchUrl } = await import("./text.ts");

test("chapterTitleText: 状态词不算章节名", () => {
  // 实测样本: 松鹤庭沐·言璃版搜「一世之尊」返回的就是这几个字
  for (const raw of ["已完结", "完结", " 连载中 ", "全本", "[已完结]", "【连载中】", "状态：已完结"]) {
    assert.equal(chapterTitleText(raw), "", `应丢弃状态词: ${raw}`);
  }
});

test("chapterTitleText: 真章节名原样保留", () => {
  assert.equal(chapterTitleText("番外：昨日星辰昨日风"), "番外：昨日星辰昨日风");
  assert.equal(chapterTitleText("  第1070章 劝阻~ "), "第1070章 劝阻~");
  assert.equal(chapterTitleText("第1章 已完结之后"), "第1章 已完结之后", "只是包含状态词, 不是状态词");
  assert.equal(chapterTitleText(undefined), "");
  assert.equal(chapterTitleText(""), "");
});

test("looksLikeSearchUrl: 关键词搜索页要认出来", () => {
  const name = "一世之尊";
  assert.equal(
    looksLikeSearchUrl(
      "https://newopensearch.reader.qq.com/wechat?keyword=%E4%B8%80%E4%B8%96%E4%B9%8B%E5%B0%8A&start=0&end=19",
      name,
    ),
    true,
  );
  assert.equal(looksLikeSearchUrl("https://newopensearch.reader.qq.com/wechat?keyword=一世之尊", name), true);
  // 真详情页: 链接里没有书名
  assert.equal(looksLikeSearchUrl("http://www.xbotaodz.info/1992.html", name), false);
  assert.equal(looksLikeSearchUrl("https://www.zzdushu.com/book/0/169/", name), false);
  assert.equal(looksLikeSearchUrl(undefined, name), false);
  assert.equal(looksLikeSearchUrl("http://x/book?keyword=一世之尊", ""), false, "无书名不猜");
});

test("looksLikeChapterTitle: 源规则拼出来的垃圾串不算章节名", () => {
  // 实测: QQ 浏览器源搜「剑来」, latestChapterTitle = "17|0|OK"
  assert.equal(looksLikeChapterTitle("17|0|OK"), false);
  assert.equal(looksLikeChapterTitle("已完结"), false, "状态词先被 chapterTitleText 滤掉");
  assert.equal(looksLikeChapterTitle(""), false);
  assert.equal(looksLikeChapterTitle(undefined), false);
  assert.equal(looksLikeChapterTitle("第84章 上升地"), true);
  assert.equal(looksLikeChapterTitle("剑来 第760节"), true);
  assert.equal(looksLikeChapterTitle("第400节"), true);
});
