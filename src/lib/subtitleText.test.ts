// Run: node src/lib/subtitleText.test.ts (Node 22.6+ runs TypeScript directly).
import { test } from "node:test";
import assert from "node:assert/strict";
import { pairBySentence, sentences, splitCjk, tidyLine } from "./subtitleText.ts";

test("lines never start with punctuation and end only on ？！", () => {
  assert.equal(tidyLine("，美国前沿公司的许多人相信。"), "美国前沿公司的许多人相信");
  assert.equal(tidyLine("。。然后呢？"), "然后呢？");
  assert.equal(tidyLine("真的吗？！"), "真的吗！");
  assert.equal(tidyLine("所以……"), "所以");
  assert.equal(tidyLine(", and then we left."), "and then we left");
  assert.equal(tidyLine("Is that right?"), "Is that right?");
  assert.equal(tidyLine("Wait!?"), "Wait?");
});

test("commas and full stops inside a Chinese line become one space", () => {
  assert.equal(tidyLine("是，美国前沿公司"), "是 美国前沿公司");
  assert.equal(tidyLine("我们有两千个代理。干得不错"), "我们有两千个代理 干得不错");
  assert.equal(tidyLine("苹果、谷歌和微软"), "苹果、谷歌和微软");
});

test("quotes and brackets whose partner is on another line go", () => {
  assert.equal(tidyLine("“我们有两千个代理"), "我们有两千个代理");
  assert.equal(tidyLine("干得不错。”"), "干得不错");
  assert.equal(tidyLine("他说“好”"), "他说“好”");
  assert.equal(tidyLine("《三体》很好看"), "《三体》很好看");
});

test("Chinese breaks between words, not inside them", () => {
  const lines = splitCjk("美国前沿公司的许多人相信第二名到第一百名都不重要", 14);
  assert.ok(lines.every((l) => [...l].length <= 14), lines.join("|"));
  assert.equal(lines.join(""), "美国前沿公司的许多人相信第二名到第一百名都不重要");
  assert.ok(!lines.some((l, i) => i > 0 && l.startsWith("司")), lines.join("|"));
  assert.ok(!lines.some((l, i) => i > 0 && /^[的了吗呢]/.test(l)), lines.join("|"));
  assert.ok(lines.every((l) => [...l].length >= 3), lines.join("|"));
});

test("Chinese prefers to break after punctuation", () => {
  const lines = splitCjk("让我们换个问题，你刚才提到的行为", 14);
  assert.equal(lines[0], "让我们换个问题，");
});

test("numerals keep their measure word; a line doesn't start with 里", () => {
  const a = splitCjk("曾这样跟我解释第二名到第一百名都无所谓", 10);
  assert.ok(!a.some((l, i) => i > 0 && /^名/.test(l)), a.join("|"));
  const b = splitCjk("原因之一是美国前沿公司里很多人相信其中一家", 10);
  assert.ok(!b.some((l, i) => i > 0 && /^里/.test(l)), b.join("|"));
});

test("sentences split at sentence ends, not inside numbers", () => {
  assert.deepEqual(sentences("It grew 1.5 times. Then it stopped!"), ["It grew 1.5 times.", "Then it stopped!"]);
  assert.deepEqual(sentences("它增长了1.5倍。然后停了！"), ["它增长了1.5倍。", "然后停了！"]);
});

test("bilingual pairs split only when both sides have the same sentences", () => {
  assert.deepEqual(pairBySentence("How far behind? Four, five months.", "落后多少？四五个月。"), [
    { orig: "How far behind?", tr: "落后多少？" },
    { orig: "Four, five months.", tr: "四五个月。" },
  ]);
  assert.equal(pairBySentence("How do they hold them back, four, five months behind?", "怎么压制他们呢？落后四五个月？"), null);
});
