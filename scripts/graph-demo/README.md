# 关系图谱 demo 数据生成管线

产出唯一交付物：`frontend/public/graph-demo-data.json`（schema `graph-demo-v1`）。
只读生产 SQLite，不改应用源码，不提交 git。

## 步骤

```bash
# 0) 只读拷贝生产库（生产库在宿主机 /home/reader/storage/reader.db，
#    由 reader-reader-1 的 bind mount 挂到容器 /storage/storage）
mkdir -p /home/hehecat/code/.tmp/gd
cp /home/reader/storage/reader.db /home/hehecat/code/.tmp/gd/reader.db

# 1) 导出全部已缓存章节 -> .tmp/gd/chapters.json
python3 01_extract_chapters.py

# 2) 采样 + 拼一次 LLM 请求体 -> .tmp/gd/req.json / sample.txt / sample_meta.json
python3 02_make_prompt.py

#    LLM 网关只在容器网络内可达；把请求体塞进容器再 curl：
docker cp /home/hehecat/code/.tmp/gd/req.json reader-reader-1:/tmp/req.json
docker exec reader-reader-1 sh -lc \
  'curl -sS "$READER_AI_BASE_URL/chat/completions" \
     -H "Authorization: Bearer $READER_AI_API_KEY" \
     -H "Content-Type: application/json" -d @/tmp/req.json' \
  > /home/hehecat/code/.tmp/gd/llm.json

# 3) 归一化 + 时间维度推导 + 自检 + 写出（读同目录 llm-output.json，可离线复跑）
python3 03_normalize.py
```

## 选书结论

- 书：《绍宋》 作者 榴弹怕水（`https://m.roushuwu.com/book/50551/`）
- `book_chapters` 有 400 行 = 395 个带章号的正文 + 5 篇卷末总结/感言
- 章序取自标题里的「第N章」（`chapter_index` 是雪花 id，无序，不可用）；1..395 连续无缺口
- `books.total_chapter_num` = 60 是脏数据（小于已缓存数），故 `meta.totalChapters` 取 395

## 两处与字面要求的偏差（均为必要性偏差）

1. `max_tokens` 仍是 8000、非流式、不设温度，但显式加了 `"reasoning_effort": "none"`。
   该网关默认开思考，8000 token 会被 reasoning 全部吃光、`content` 为空（实测
   `finish_reason=length`、`completion_thinking_tokens=8000`、content 长度 0）。
2. 除任务书列的归一化外，多加了一步「用模型自己给出的 quote 做亲缘体检」：
   parent 方向写反（引用只有「的爹」而无「长子/儿子」）就翻转；把「侄子/叔父」误标成
   parent/sibling/spouse 的降级为 other。仍是规则化、不看具体人名。

## 已知瑕疵

见交付报告。

## v2：多批抽取（当前管线）

数据从 65 人/81 关系扩到 138 人/267 关系，靠的是多批抽取 + 归一化合并，参数都写死在脚本顶部。
`01_extract_chapters.py` 不用重跑（`chapters.json` 已在 `.tmp/gd/`）。

```bash
# 2) 生成 23 个请求体（.tmp/gd/batches/req_NN.json）
python3 02_make_prompt.py
#    然后逐批 curl（key 只从只读库 argparse 取，别写进脚本/日志）
bash /home/hehecat/code/.tmp/gd/run_batch.sh 01
# 3) 合并全部 batches/llm_*.json -> 交付物
python3 03_normalize.py
# 4) 硬指标自检（失败非零退出）
python3 04_verify.py
```

`02_make_prompt.py` 的批次构成：

| 批 | 内容 | 关键参数 |
| --- | --- | --- |
| req_01–06 | 6 个重叠窗口扫全书 395 章，每窗取 40 章 | `BATCHES=6 / WIN_LEN=105 / PER_BATCH=40 / EXCERPT=2800 / BUDGET=115000 / MAXTOK=16000` |
| req_07 | 只抽亲缘（parent/spouse/sibling/master），按「长子/之子/女婿/侄子…」句式密度选章 | `KIN_CUE` 权重 4 |
| req_08 | 常胜军/郭药师 专批（正文只出现 6 章） | ±2 章窗口 |
| req_09–18 | 第一轮没覆盖到的章，按人名密度降序补齐（40 章/批） | 全覆盖 395 章 |
| req_19–23 | 军营/朝堂/同盟偏置补抽，只抽 ally/enemy/other | `NO_PROTO` |

选章打分：`人名命中数 × 1000 / 字数`，已采章降权 6 倍，每窗首尾各强制留 1 章。

`03_normalize.py` 的合并与清洗（顺序即代码顺序）：

1. 端点解析 → 同人合并（别名 owner 唯一，一符多主则丢弃该别名；别名若是别人的正名也不算同一人）
2. 出处核验：quote 回正文查，章号改成真实章号，查不到即丢
3. 亲缘出处修复：quote 里没有亲缘线索的，回正文找≤20 字、同一句、同时套住两端的片段替换；找不到才丢
4. 类型纠偏：master 无「恩师/弟子/拜师」字样降级 other；parent 无「之子/其父/长子」线索丢弃；
   ally 无「同盟/结盟/联手」线索丢弃；other 无「主公/部将/麾下」线索丢弃
5. 去重：无向类型（sibling/spouse/ally/enemy）A→B 与 B→A 视为同一条
6. generation：加权并查集（父子差 1 优先级最高，夫妻/兄弟同代，冲突边丢弃），每个亲缘连通块从 0 起跳
7. 规模收敛：`TARGET_CHARS=133 / MAX_CHARS=139`，按「度数 + role + 出场章数」打分，但亲缘边上的人一律保住；
   单点度数上限 `MAX_DEGREE_RATIO≈12%`；孤点直接剔除；关系总数 `MAX_RELATIONS=300`
8. 势力：`MAX_FACTIONS=10`，人物 faction 值 + role 里的势力名 + 多批声明的 leader 一起投票，小势力靠 `SEED_FACTIONS` 兜底

## v3：关系方向与类型的成对校正（当前）

多批抽取的弱点是**方向**：模型经常把「其父X」「生母X」「他岳父X」写成了「主角 → X」。

`03_normalize.py` 因此加了三张「无向对」人工校正表（键都是人名对，不区分方向），外加
一条合并表和一个 `must_keep` 白名单：

| 常量 | 作用 | 当前内容 |
| --- | --- | --- |
| `KIN_FLIP` | 纠正 父 → 子 的方向（并据此重算 generation） | 刘韐—刘子羽、韦太后—赵玖、完颜阿骨打—完颜兀术 |
| `KIN_RETYPE` | 不是亲子，改类型 + 带 label | 赵玖—郑太后（嫡母）、完颜挞懒—蒲察鹘拔鲁（女婿）、薛元礼—高守义（岳父） |
| `KIN_LABEL` | 类型不动、只改 label | 张显/汤怀/王贵—岳飞（结义兄弟） |
| `FORCE_MERGE` | 异写同一人强制合并 | 完颜拔里速 → 完颜拔离速 |
| `KEEP_CHARS` | 校正过的人进 `prune_for_demo` 的 must_keep | 校正后边变 other 会被打分裁掉，不保就整人消失 |

被校正的边带 `pin` 标记：裁剪排序与二次去重优先保留，免得「女婿」这条被更长的旧 quote 顶掉。

`04_verify.py` 新增第 11 项断言 —— **parent 边方向必须与 quote 里的亲缘字眼一致**：

- 「其父/乃父/生父/其母/生母/父亲/家父/爹爹」后面紧跟的名字必须是父/母端
- 「长子/次子/幼子/其子/之子/亲女/女儿/其女」前面紧邻是父/母端、后面紧邻是子女端
- quote 里出现「女婿/岳父/岳丈/嫡母/继母/庶母/正宫/侄/外甥/族兄/堂兄/表兄/乳母」直接失败
  （唯一白名单：`吴近 -> 吴夫人`，quote「吴氏岳父吴近」是赵玖视角，吴近确为吴夫人之父）

拿 v2 的产物反向跑这条断言，能精准报出上面节 5 条错边，所以它现在能当回归门用。

当前产物：**138 人物 / 265 关系 / 10 势力**，sha256 见交付报告；`04_verify.py` 全绿。
