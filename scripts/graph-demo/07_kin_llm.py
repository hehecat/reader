#!/usr/bin/env python3
"""v2 专批：亲缘/师承补抽（第三批 prompt）——只把「带亲缘字眼」的句子切成若干批，
连同图谱人物名单一起交给模型，要求它给出 (甲, 乙, 类型, 方向, 逐字 quote)。

模型只负责提名，落盘前一律过 verify_quote（逐字、<=20 字），过不了的丢弃。
"""
import json, os, re, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from gdlib import Chapters, BOOK_JSON, V2

CUE = re.compile(r"之子|长女|次女|女儿|闺女|亲女|长兄|次兄|兄长|弟弟|哥哥|兄弟|胞弟|亲弟|族兄|堂兄|"
                 r"父亲|家父|其父|乃父|生父|生母|其母|爹爹|老爹|阿爹|母亲|叔父|伯父|祖父|祖母|"
                 r"女婿|岳父|岳丈|妻|夫人|娘子|娶|嫁|成亲|为后|贵妃|妃子|大婚|夫君|公主|皇子|所出|"
                 r"恩师|师父|师从|弟子|徒弟|门生|拜师|受业|学于|从学|高足|嫡传")
CH_PER_BATCH = 14
CHARS_PER_BATCH = 5200


def main():
    chap = Chapters()
    data = json.load(open(BOOK_JSON))
    names = [c["name"] for c in data["characters"]]
    # 收集「亲缘字眼句」+ 前后各一句（保上下文）
    keep = []
    for n in chap.ns:
        sents = chap.sentences(n)
        idx = [i for i, (s, e, seg) in enumerate(sents) if CUE.search(seg)]
        if not idx:
            continue
        take = sorted({j for i in idx for j in (i - 1, i, i + 1) if 0 <= j < len(sents)})
        txt = "".join(sents[j][2] + "。" for j in take)
        if any(nm in txt for nm in names):
            keep.append((n, txt))
    batches, buf, chs, size = [], [], [], 0
    for n, txt in keep:
        if size + len(txt) > CHARS_PER_BATCH and buf:
            batches.append((chs, buf))
            buf, chs, size = [], [], 0
        buf.append(f"[第{n}章] {txt}")
        chs.append(n)
        size += len(txt)
    if buf:
        batches.append((chs, buf))

    outdir = os.path.join(V2, "req")
    os.makedirs(outdir, exist_ok=True)
    sys_prompt = ("你在从中文历史小说《绍宋》正文里抽取人物亲属与师承关系。只能使用文中确实写出的关系，"
                  "绝不推断、绝不虚构。")
    listed = set(names)
    for i, (chs, buf) in enumerate(batches, 1):
        prompt = (
            "下面是《绍宋》若干章里含亲属/师承字眼的句子（已标章号）。请找出其中【两人之间】的亲属或师承关系。\n"
            f"图谱里已有的人物名（请严格沿用这些写法）：{'、'.join(listed)}\n"
            "要求：\n"
            "1) 只输出两人的名字都在上面名单里的关系；名单外的人不要输出。\n"
            "2) type 取 parent（甲是乙的父亲或母亲，方向必须是长辈->晚辈）/ sibling（兄弟姐妹）/ "
            "spouse（夫妻）/ master（甲是乙的师父）/ disciple（甲是乙的弟子）。\n"
            "3) quote 必须是正文里连续的一段原文，≤20 个汉字，且能看出这层关系（含「其父/长子/之弟/妻/恩师/弟子」等字眼）。"
            "不许改写、不许拼接、不许加省略号。\n"
            "4) 岳父/女婿这类姻亲不要输出（另有处理）。同一对人只输出一次。\n"
            "只输出 JSON 数组：[{\"甲\":\"\",\"乙\":\"\",\"type\":\"parent\",\"quote\":\"\",\"章\":123}]\n\n"
            "正文：\n" + "\n".join(buf)
        )
        req = {"model": "deepseek-v4.1-flash", "reasoning_effort": "none", "max_tokens": 8000,
               "messages": [{"role": "system", "content": sys_prompt},
                            {"role": "user", "content": prompt}]}
        json.dump(req, open(os.path.join(outdir, f"kin_extract_{i:02d}.json"), "w"),
                  ensure_ascii=False)
        print(f"batch {i:02d}: chapters {chs[0]}-{chs[-1]} chars={sum(len(x) for x in buf)}")
    print("kin extract batches:", len(batches))


if __name__ == "__main__":
    main()
