import { CircleCheck, TriangleAlert } from "lucide-react";
import * as React from "react";

import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Switch,
  Tabs,
  TabsList,
  TabsTrigger,
  Textarea,
  cn,
  toast,
} from "@/components/ui";
import { getRssArticles, rssSourceSchema, type RssSource } from "@/services/rss";

import { rssErrorMessage, useSaveRssSource } from "./useRss";

export interface EditRssSourceDialogProps {
  /** 编辑的订阅; null 表示新增 */
  source: RssSource | null;
  /** 已保存的全部订阅: 用于判断同地址覆盖 */
  sources: readonly RssSource[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/** 新增订阅的空白草稿(与 rssSourceSchema 输出类型一致, 文本字段全为 "") */
function emptyDraft(): RssSource {
  return {
    sourceUrl: "",
    sourceName: "",
    sourceIcon: "",
    sourceGroup: "",
    sourceComment: "",
    enabled: true,
    concurrentRate: "",
    header: "",
    loginUrl: "",
    loginCheckJs: "",
    sortUrl: "",
    singleUrl: false,
    articleStyle: 0,
    ruleArticles: "",
    ruleNextPage: "",
    ruleTitle: "",
    rulePubDate: "",
    ruleDescription: "",
    ruleImage: "",
    ruleLink: "",
    ruleContent: "",
    style: "",
    enableJs: true,
    loadWithBaseUrl: true,
    customOrder: 0,
  };
}

function Field({
  label,
  htmlFor,
  className,
  error,
  children,
}: {
  label: string;
  htmlFor?: string;
  className?: string;
  /** 校验文案; null 时不渲染(调用方负责只在 touched/提交后才给值) */
  error?: string | null;
  children: React.ReactNode;
}) {
  return (
    <div className={cn("flex flex-col gap-1", className)}>
      <label htmlFor={htmlFor} className="text-xs font-medium text-muted-foreground">
        {label}
      </label>
      {children}
      {error !== undefined && error !== null ? (
        <p role="alert" className="text-xs text-danger">
          {error}
        </p>
      ) : null}
    </div>
  );
}

/** 订阅地址必须是 http(s) 绝对地址: 后端按该 URL 抓取 Feed, 相对地址与其它协议都无从解析 */
function isValidSourceUrl(value: string): boolean {
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

/** 「试解析」结果: 绑定发起时的地址, 地址一改即自动过期隐藏 */
type TestOutcome = { url: string } & ({ ok: true; count: number } | { ok: false; message: string });

/**
 * 添加/编辑订阅: 「表单」页编辑关键字段, 「JSON」页整对象编辑, 两页共享同一份草稿.
 * 后端按 sourceUrl upsert, 修改地址会另存为一个新订阅(同地址命中其它订阅时给出覆盖提示).
 * 校验只在字段失焦或点过保存后露面; 「试解析」按服务端已保存的订阅抓一次首页文章,
 * 让地址/规则的问题在保存前后都能就地看到, 而不是只能等文章列表报错.
 */
export function EditRssSourceDialog({
  source,
  sources,
  open,
  onOpenChange,
}: EditRssSourceDialogProps) {
  const [tab, setTab] = React.useState("form");
  const [draft, setDraft] = React.useState<RssSource>(emptyDraft);
  const [jsonText, setJsonText] = React.useState("");
  const [jsonError, setJsonError] = React.useState<string | null>(null);
  const [submitted, setSubmitted] = React.useState(false);
  const [touched, setTouched] = React.useState({ sourceName: false, sourceUrl: false });
  const [testing, setTesting] = React.useState(false);
  const [testOutcome, setTestOutcome] = React.useState<TestOutcome | null>(null);
  const save = useSaveRssSource();
  const isNew = source === null;

  // 每次打开时重置草稿与校验/试解析状态(编辑回填当前订阅, 新增给空白)
  React.useEffect(() => {
    if (!open) return;
    const next = source !== null ? { ...source } : emptyDraft();
    setDraft(next);
    setJsonText(JSON.stringify(next, null, 2));
    setJsonError(null);
    setSubmitted(false);
    setTouched({ sourceName: false, sourceUrl: false });
    setTesting(false);
    setTestOutcome(null);
    setTab("form");
  }, [open, source]);

  /** 表单页修改: 同步刷新 JSON 页文本 */
  const patchDraft = (patch: Partial<RssSource>) => {
    setDraft((prev) => {
      const next = { ...prev, ...patch };
      setJsonText(JSON.stringify(next, null, 2));
      return next;
    });
  };

  /** JSON 页修改: 解析成功才更新草稿, 失败保留文本并给出错误 */
  const handleJsonChange = (value: string) => {
    setJsonText(value);
    try {
      const raw: unknown = JSON.parse(value);
      if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
        setJsonError("需要一个订阅 JSON 对象");
        return;
      }
      const parsed = rssSourceSchema.safeParse(raw);
      if (!parsed.success) {
        setJsonError("字段校验失败: 检查字段类型");
        return;
      }
      setDraft(parsed.data);
      setJsonError(null);
    } catch (error) {
      setJsonError(`JSON 解析失败: ${error instanceof Error ? error.message : String(error)}`);
    }
  };

  const url = draft.sourceUrl.trim();
  const nameError = draft.sourceName.trim().length === 0 ? "订阅名称不能为空" : null;
  const urlError =
    url.length === 0
      ? "订阅地址 (sourceUrl) 不能为空"
      : isValidSourceUrl(url)
        ? null
        : "订阅地址需要是 http(s) 完整地址, 例如 https://example.com/feed.xml";
  // 校验文案始终算出来(保存拦截与 JSON 页要用), 但只在失焦/提交后展示
  const nameFeedback = submitted || touched.sourceName ? nameError : null;
  const urlFeedback = submitted || touched.sourceUrl ? urlError : null;
  const formFeedback = urlError ?? nameError;
  const jsonFeedback = jsonError ?? (submitted ? formFeedback : null);
  const canSave = formFeedback === null && jsonError === null;

  // 同地址的其它已保存订阅: 后端按 sourceUrl upsert, 保存即覆盖它
  const overwriteName = React.useMemo(() => {
    if (url.length === 0 || url === source?.sourceUrl) return null;
    const clash = sources.find((item) => item.sourceUrl === url);
    if (clash === undefined) return null;
    return clash.sourceName.trim().length > 0 ? clash.sourceName : clash.sourceUrl;
  }, [sources, source, url]);

  const testResult = testOutcome !== null && testOutcome.url === url ? testOutcome : null;

  /** 抓一次首页文章验证地址与规则: 单次请求, 不翻页、不自动重试 */
  const runTestParse = async () => {
    if (url.length === 0 || testing) return;
    setTesting(true);
    setTestOutcome(null);
    try {
      const page = await getRssArticles({ sourceUrl: url, sortName: "", sortUrl: "", page: 1 });
      setTestOutcome({ url, ok: true, count: page.first.length });
    } catch (error) {
      setTestOutcome({ url, ok: false, message: rssErrorMessage(error, "试解析失败") });
    } finally {
      setTesting(false);
    }
  };

  const handleSave = () => {
    // 不静默失败: 拦下保存的同时把校验文案亮出来
    if (!canSave) {
      setSubmitted(true);
      return;
    }
    save.mutate(draft, {
      onSuccess: () => {
        toast.success(isNew ? "已添加订阅" : "已保存订阅");
        onOpenChange(false);
      },
    });
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent width="xl" className="md:max-h-[85vh]">
        <DialogHeader>
          <DialogTitle>{isNew ? "添加订阅" : "编辑订阅"}</DialogTitle>
          <DialogDescription>
            {isNew
              ? "填写 RSS 订阅的地址与解析规则; 规则留空时由后端按标准 Feed 解析, 保存后可用「试解析」验证"
              : "按 sourceUrl 覆盖保存; 修改地址会另存为一个新订阅, 「试解析」按服务端已保存的规则抓取"}
          </DialogDescription>
        </DialogHeader>

        <Tabs
          value={tab}
          onValueChange={setTab}
          className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-4 md:overflow-visible md:px-5"
        >
          <TabsList className="shrink-0 self-start">
            <TabsTrigger value="form">表单</TabsTrigger>
            <TabsTrigger value="json">JSON</TabsTrigger>
          </TabsList>

          {tab === "form" ? (
            <div className="flex min-h-0 flex-col gap-4 md:overflow-y-auto md:pr-1">
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                <Field label="名称 *" htmlFor="rss-source-name" error={nameFeedback}>
                  <Input
                    id="rss-source-name"
                    value={draft.sourceName}
                    placeholder="例如: 阮一峰的网络日志"
                    invalid={nameFeedback !== null}
                    onChange={(event) => patchDraft({ sourceName: event.target.value })}
                    onBlur={() => setTouched((prev) => ({ ...prev, sourceName: true }))}
                  />
                </Field>
                <Field label="分组" htmlFor="rss-source-group">
                  <Input
                    id="rss-source-group"
                    value={draft.sourceGroup}
                    placeholder="可选, 用于列表徽标"
                    onChange={(event) => patchDraft({ sourceGroup: event.target.value })}
                  />
                </Field>
              </div>
              <Field label="订阅地址 *" htmlFor="rss-source-url" error={urlFeedback}>
                <Input
                  id="rss-source-url"
                  value={draft.sourceUrl}
                  placeholder="https://example.com/feed.xml"
                  invalid={urlFeedback !== null}
                  onChange={(event) => patchDraft({ sourceUrl: event.target.value })}
                  onBlur={() => setTouched((prev) => ({ ...prev, sourceUrl: true }))}
                />
                <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1">
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={!isValidSourceUrl(url)}
                    loading={testing}
                    title="按服务端已保存的订阅与规则抓取首页文章"
                    onClick={() => {
                      void runTestParse();
                    }}
                  >
                    试解析
                  </Button>
                  {testResult === null ? null : testResult.ok ? (
                    <span className="flex items-center gap-1 text-xs text-muted-foreground tabular-nums">
                      <CircleCheck aria-hidden className="size-3.5 shrink-0 text-accent" />
                      解析到 {testResult.count} 篇文章
                    </span>
                  ) : (
                    <span role="alert" className="text-xs text-danger">
                      {testResult.message}
                    </span>
                  )}
                </div>
                {overwriteName === null ? null : (
                  <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
                    <TriangleAlert aria-hidden className="mt-px size-3.5 shrink-0" />
                    将覆盖同地址的现有订阅「{overwriteName}」
                  </p>
                )}
              </Field>
              <Field label="备注" htmlFor="rss-source-comment">
                <Textarea
                  id="rss-source-comment"
                  autoSize
                  value={draft.sourceComment}
                  placeholder="可选"
                  onChange={(event) => patchDraft({ sourceComment: event.target.value })}
                />
              </Field>

              {/* 规则是给抓取器看的字段, 不是日常要填的东西: 默认收起, 留空走 RSS/Atom 标准解析 */}
              <details className="rounded-lg border border-border/70 px-3 py-2">
                <summary className="cursor-pointer text-sm font-medium">
                  解析规则(高级, 可选)
                </summary>
                <p className="mt-1 text-xs text-muted-foreground">
                  留空则按 RSS/Atom 标准字段解析; 自定义页面才需要逐项填规则.
                </p>
                <fieldset className="mt-3 flex flex-col gap-3">
                <Field label="列表规则" htmlFor="rss-rule-articles">
                  <Input
                    id="rss-rule-articles"
                    value={draft.ruleArticles}
                    placeholder="例如: tag.item 或 @css:.feed-entry"
                    onChange={(event) => patchDraft({ ruleArticles: event.target.value })}
                  />
                </Field>
                <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                  <Field label="标题" htmlFor="rss-rule-title">
                    <Input
                      id="rss-rule-title"
                      value={draft.ruleTitle}
                      placeholder="例如: tag.title@text"
                      onChange={(event) => patchDraft({ ruleTitle: event.target.value })}
                    />
                  </Field>
                  <Field label="链接" htmlFor="rss-rule-link">
                    <Input
                      id="rss-rule-link"
                      value={draft.ruleLink}
                      placeholder="例如: tag.link@text"
                      onChange={(event) => patchDraft({ ruleLink: event.target.value })}
                    />
                  </Field>
                  <Field label="发布时间" htmlFor="rss-rule-pubdate">
                    <Input
                      id="rss-rule-pubdate"
                      value={draft.rulePubDate}
                      placeholder="例如: tag.pubDate@text"
                      onChange={(event) => patchDraft({ rulePubDate: event.target.value })}
                    />
                  </Field>
                  <Field label="描述" htmlFor="rss-rule-description">
                    <Input
                      id="rss-rule-description"
                      value={draft.ruleDescription}
                      placeholder="例如: tag.description@text"
                      onChange={(event) => patchDraft({ ruleDescription: event.target.value })}
                    />
                  </Field>
                  <Field label="图片" htmlFor="rss-rule-image">
                    <Input
                      id="rss-rule-image"
                      value={draft.ruleImage}
                      placeholder="例如: tag.enclosure@url"
                      onChange={(event) => patchDraft({ ruleImage: event.target.value })}
                    />
                  </Field>
                  <Field label="下一页" htmlFor="rss-rule-nextpage">
                    <Input
                      id="rss-rule-nextpage"
                      value={draft.ruleNextPage}
                      placeholder="PAGE 或下一页链接规则"
                      onChange={(event) => patchDraft({ ruleNextPage: event.target.value })}
                    />
                  </Field>
                </div>
                <Field label="正文" htmlFor="rss-rule-content">
                  <Input
                    id="rss-rule-content"
                    value={draft.ruleContent}
                    placeholder="例如: class.article-content@html; 留空则阅读对话框提示查看原文"
                    onChange={(event) => patchDraft({ ruleContent: event.target.value })}
                  />
                </Field>
                <Field label="分类" htmlFor="rss-sort-url">
                  <Textarea
                    id="rss-sort-url"
                    autoSize
                    value={draft.sortUrl}
                    placeholder={"每行一个分类, 格式 名称::地址"}
                    onChange={(event) => patchDraft({ sortUrl: event.target.value })}
                  />
                </Field>
                </fieldset>
              </details>

              <div className="flex items-center justify-between rounded-lg border px-3 py-2.5">
                <div className="min-w-0">
                  <p className="text-sm font-medium">启用</p>
                  <p className="text-xs text-muted-foreground">停用后仍保留订阅与规则</p>
                </div>
                <Switch
                  checked={draft.enabled}
                  aria-label="启用订阅"
                  onCheckedChange={(enabled) => patchDraft({ enabled })}
                />
              </div>
            </div>
          ) : (
            <div className="flex min-h-0 flex-1 flex-col gap-2">
              <Textarea
                aria-label="订阅 JSON"
                className="min-h-64 flex-1 font-mono text-xs leading-5"
                value={jsonText}
                spellCheck={false}
                invalid={jsonFeedback !== null}
                onChange={(event) => handleJsonChange(event.target.value)}
              />
              {jsonFeedback !== null ? (
                <p role="alert" className="text-xs text-danger">
                  {jsonFeedback}
                </p>
              ) : null}
            </div>
          )}
        </Tabs>

        {tab === "form" && jsonError !== null ? (
          <p role="alert" className="px-4 text-xs text-danger md:px-5">
            JSON 页有未修正的错误: {jsonError}
          </p>
        ) : null}

        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button loading={save.isPending} onClick={handleSave}>
            {isNew ? "添加" : "保存"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
