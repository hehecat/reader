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

## v4：敌对复核专批 + 势力收口 + 亲缘召回（当前）

v3 产物虽然自检全绿，但数据本身有三个硬伤（父 agent 点名）：

1. **enemy 占比 59.6%（158/265）**：诊断发现 158 条里有 99 条是**同一势力内部**的
   （赵玖—黄潜善、张浚—李纲……），把「君臣议事/政见之争/弹劾」误标成了敌对；
2. **势力碎**：赤心队/常胜军/梁山泊/八字军 各 1 人，12 人 `faction=null`；
3. **三处别名并错**：胡闳休→胡寅、刘正彦→刘光世、王彦章→王彦（王彦章全书 0 次出现，纯臆造）。

同时亲缘/师承太少：parent 23、master+disciple 7、spouse 5。

### 新增的四个脚本（都在本目录，读 `.tmp/graph-data-v2/` 的中间产物）

| 脚本 | 干什么 |
| --- | --- |
| `05_review_batches.py` | 把 149 条「有同句共现」的 enemy 边切成 7 批送模型复核（每批 26 条，附双方 role/faction/同句片段），另把 12 个 null + 郭药师 送 1 批问势力归属 |
| `06_kin_mine.py` | 纯规则亲缘挖掘（人名表 + 亲缘字眼 ±20 字窗口），做召回对照 |
| `07_kin_llm.py` | 39 批「亲缘字眼句 ± 上下句」专批，问 parent/sibling/spouse/master/disciple（附 138 人名单，禁止输出姻亲） |
| `08_build_overrides.py` | 把复核结论 + 逐条核过正文的人工校正合成 `v2_overrides.json`；**任何 quote 过不了 `verify_quote` 就不写入**（计入 gaps） |

### 复核专批的提示词关键约束（这是把 59.6% 打到 16.6% 的真正原因）

- 枚举里明确写死：`other` = 君臣 / 上下级 / 部将主公 / 同僚 / 旧识 / 姻亲 / **政见之争（弹劾、训斥、看不顺眼都算这里）**；
- 「片段没写敌对行为就不要再标 enemy」「不要因为分属宋金就判敌对」；
- quote 必须在片段内连续、≤20 汉字、不许改写/拼接/省略号。

149 条复核结果：**other 107、enemy 38、ally 4**（另有 9 条无同句共现未送审，保持原样）。

### `03_normalize.py` 的四处改动

1. `BLOCKED_ALIASES`：三处错误别名在**并查集合并之前**就摘掉（否则先并进胡寅/刘光世，后面拆不回来），胡闳休、刘正彦 因此恢复成独立人物；
2. 新增覆盖层 `v2_overrides.json`（`retype` / `keep_enemy` / `add_characters` / `add_relations` / `faction` / `faction_leader` / `drop_relations`），落在「再压一次重复」之后、generation 之前，生效的边带 `pin`；
3. 势力收口：赤心队→御营、常胜军→金国，`MAX_FACTIONS=9`，`SEED_FACTIONS` 改成 八字军/梁山泊；
4. `GRAPH_DEMO_OUT` 环境变量可把产物写到别处（自检通过前不碰 `frontend/public`）。

### 结果（v3 → v4）

| 维度 | v3 | v4 |
| --- | --- | --- |
| 人物 / 关系 / 势力 | 138 / 265 / 10 | 139 / 277 / 8 |
| enemy | 158（59.6%） | 46（**16.6%**） |
| parent · sibling · spouse | 23 · 23 · 5 | **31 · 25 · 6** |
| master+disciple | 7 | **20**（10 对师承双向编码） |
| 亲缘合计 | 58 | 82 |
| faction=null | 12 | 3（钟相/耶律大石/合不勒，正文里确实不属任何势力） |
| 单成员小势力 | 4 | 0 |
| 最大度数占比 | 11.3% | 10.8% |

`04_verify.py` 追加 10 条 v2 断言（enemy ≤40%、parent ≥30、master+disciple ≥18、sibling ≥20、
spouse ≥6、关系 240–300、人物 130–150、势力 6–9、无单成员小势力、null ≤3），原 40 余条断言未动。
