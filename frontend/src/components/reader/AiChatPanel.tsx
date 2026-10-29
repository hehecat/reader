import { Bot, Send, Square, Trash2, X } from "lucide-react";
import * as React from "react";

import {
  Badge,
  Button,
  IconButton,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
  Textarea,
  cn,
} from "@/components/ui";
import { MarkdownLite } from "@/components/reader/ai-markdown";
import { GraphTabCard } from "@/components/reader/GraphTabCard";
import { streamAiChat, type AiScope, type AiSnippet } from "@/services/ai";

/** 检索范围偏好：本地记住，下次打开面板还是上次选的 */
const SCOPE_KEY = "reader.aiScope";
const SCOPE_OPTIONS: { id: AiScope; label: string; hint: string }[] = [
  // 说明压在一行内: 手机 390px 下面板底部只有 ~200px 可用, 写长会折行把发送按钮挤歪
  { id: "current", label: "当前章", hint: "当前章及之前（默认）" },
  { id: "book", label: "全书", hint: "跨章检索，线索更全，更耗 token" },
];

function readScope(): AiScope {
  try {
    if (localStorage.getItem(SCOPE_KEY) === "book") {
      return "book";
    }
  } catch {
    // 隐私模式下 localStorage 可能不可用，忽略
  }
  return "current";
}

interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  text: string;
  kind: "text" | "error";
  /** 本次回答引用的章节（后端检索命中的片段） */
  snippets: AiSnippet[];
  /** 流式进行中 */
  pending?: boolean;
  /** 推理模型的思考进度（字符数） */
  thinking?: number;
  /** 过程提示（自动续写等） */
  notice?: string;
  /** 回答达到 max_tokens 被截断（可继续生成） */
  truncated?: boolean;
}

export interface AiChatPanelProps {
  open: boolean;
  onClose: () => void;
  bookUrl: string;
  bookName: string;
  /** 章节标题序 + 当前章：后端据此定位「当前章及之前」的检索范围 */
  titles: string[];
  currentTitle: string;
  /** 是否已配置模型端点（false 时面板只显示配置提示） */
  configured: boolean;
  model?: string | null;
  /** 点击引用章节：页面按标题反查章节索引并跳转 */
  onJump: (title: string) => void;
}

const EXAMPLES = [
  "主要人物有哪些, 关系如何?",
  "主角第一次得到关键物品是哪一章?",
  "最近几章发生了什么?",
];

let nextId = 1;

/**
 * 右侧 AI 阅读助手面板：
 * - 桌面为正文右侧固定栏（md:w-96），移动端为全屏覆盖；
 * - 回答基于「已缓存章节」检索片段，答案附章节引用；点击引用跳章；
 * - 未配置模型端点时显示配置说明（READER_AI_BASE_URL / READER_AI_API_KEY）。
 */
export function AiChatPanel({
  open,
  onClose,
  bookUrl,
  bookName,
  titles,
  currentTitle,
  configured,
  model,
  onJump,
}: AiChatPanelProps) {
  const [tab, setTab] = React.useState<"chat" | "graph">("chat");
  const [messages, setMessages] = React.useState<ChatMessage[]>([]);
  const [input, setInput] = React.useState("");
  const [streaming, setStreaming] = React.useState(false);
  const [scope, setScope] = React.useState<AiScope>(readScope);
  // 关系图: 生成结果 / 加载态 / 错误
  const cancelRef = React.useRef<(() => void) | null>(null);
  const scrollRef = React.useRef<HTMLDivElement>(null);

  // 换书时清空对话（同一本书关掉面板再打开保留上下文）
  const bookRef = React.useRef(bookUrl);
  if (bookRef.current !== bookUrl) {
    bookRef.current = bookUrl;
    cancelRef.current?.();
    cancelRef.current = null;
  }

  React.useEffect(
    () => () => {
      cancelRef.current?.();
    },
    [],
  );

  // 新消息 / 流式增量时贴底
  React.useEffect(() => {
    const el = scrollRef.current;
    if (el !== null) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages]);

  const patch = React.useCallback((id: number, updater: (m: ChatMessage) => ChatMessage) => {
    setMessages((prev) => prev.map((m) => (m.id === id ? updater(m) : m)));
  }, []);

  const send = React.useCallback(
    (override?: string) => {
    const question = (override ?? input).trim();
    if (question === "" || streaming || !configured) {
      return;
    }
    const history = messages
      .filter((m) => m.kind === "text" && m.text.trim() !== "")
      .slice(-8)
      .map((m) => ({ role: m.role, content: m.text }));
    const userMsg: ChatMessage = {
      id: nextId++,
      role: "user",
      text: question,
      kind: "text",
      snippets: [],
    };
    const botMsg: ChatMessage = {
      id: nextId++,
      role: "assistant",
      text: "",
      kind: "text",
      snippets: [],
      pending: true,
    };
    setMessages((prev) => [...prev, userMsg, botMsg]);
    if (override === undefined) {
      setInput("");
    }
    setStreaming(true);

    cancelRef.current = streamAiChat(
      { url: bookUrl, question, bookName, scope, titles, currentTitle, history },
      {
        onMeta: ({ snippets }) => {
          patch(botMsg.id, (m) => ({ ...m, snippets }));
        },
        onNotice: (message) => {
          patch(botMsg.id, (m) => ({ ...m, notice: message }));
        },
        onThinking: (chars) => {
          patch(botMsg.id, (m) => ({ ...m, thinking: chars }));
        },
        onDelta: (text) => {
          patch(botMsg.id, (m) => ({ ...m, text: m.text + text, thinking: 0 }));
        },
        onDone: (finish) => {
          setStreaming(false);
          cancelRef.current = null;
          patch(botMsg.id, (m) => ({
            ...m,
            pending: false,
            thinking: 0,
            truncated: finish === "length",
          }));
        },
        onError: (message) => {
          setStreaming(false);
          cancelRef.current = null;
          patch(botMsg.id, (m) => ({
            ...m,
            pending: false,
            thinking: 0,
            kind: m.text === "" ? "error" : "text",
            text: m.text === "" ? message : m.text,
          }));
        },
      },
    );
    },
    [bookName, bookUrl, configured, currentTitle, input, messages, patch, scope, streaming, titles],
  );

  const stop = React.useCallback(() => {
    cancelRef.current?.();
    cancelRef.current = null;
    setStreaming(false);
    setMessages((prev) =>
      prev.map((m) => (m.pending ? { ...m, pending: false, thinking: 0 } : m)),
    );
  }, []);

  const clear = React.useCallback(() => {
    cancelRef.current?.();
    cancelRef.current = null;
    setStreaming(false);
    setMessages([]);
  }, []);

  return (
    <aside
      role="complementary"
      aria-label="AI 阅读助手"
      className={cn(
        "z-40 flex h-full min-h-0 flex-col border-border bg-surface",
        "absolute inset-0 w-full md:static md:z-auto md:w-96 md:shrink-0 md:border-l",
        open ? "flex" : "hidden",
      )}
    >
      <header className="ui-safe-t flex h-14 shrink-0 items-center gap-2 border-b border-border/70 px-3">
        <Bot aria-hidden className="size-4 shrink-0 text-accent" />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-semibold">AI 阅读助手</p>
          <p className="truncate text-xs text-muted-foreground">
            {configured ? (model ?? "已缓存章节问答") : "未配置模型"}
          </p>
        </div>
        <IconButton variant="ghost" size="sm" tooltip="清空对话" onClick={clear}>
          <Trash2 />
        </IconButton>
        <IconButton variant="ghost" size="sm" tooltip="关闭" onClick={onClose}>
          <X />
        </IconButton>
      </header>

      {configured ? (
        <div className="flex shrink-0 items-center gap-1 border-b border-border/70 px-2 py-1.5">
          <Button
            size="sm"
            variant={tab === "chat" ? "secondary" : "ghost"}
            onClick={() => setTab("chat")}
          >
            对话
          </Button>
          <Button
            size="sm"
            variant={tab === "graph" ? "secondary" : "ghost"}
            onClick={() => setTab("graph")}
          >
            人物关系
          </Button>
        </div>
      ) : null}

      {!configured ? (
        <div className="px-4 py-6 text-sm text-muted-foreground">
          未配置 AI 模型。在部署目录的 <code className="font-mono text-xs">.env</code> 里设置{" "}
          <code className="font-mono text-xs">READER_AI_BASE_URL</code> 与{" "}
          <code className="font-mono text-xs">READER_AI_API_KEY</code> 后重启容器即可。
        </div>
      ) : tab === "graph" ? (
        <GraphTabCard bookUrl={bookUrl} bookName={bookName} />
      ) : (
        <>
          <div ref={scrollRef} className="min-h-0 flex-1 space-y-3 overflow-y-auto px-3 py-3">
            {messages.length === 0 ? (
              <div className="pt-4">
                <p className="text-sm text-muted-foreground">
                  问我关于《{bookName || "本书"}》的情节：已缓存的章节我都能查，回答会标注出处。
                </p>
                <div className="mt-3 flex flex-col gap-2">
                  {EXAMPLES.map((example) => (
                    <Button
                      key={example}
                      variant="secondary"
                      size="sm"
                      className="justify-start text-left"
                      onClick={() => setInput(example)}
                    >
                      {example}
                    </Button>
                  ))}
                </div>
                <p className="mt-4 text-xs text-muted-foreground/80">
                  只检索本机已缓存的章节；读得越多，能回答的越多。
                </p>
              </div>
            ) : (
              messages.map((m) => (
                <div
                  key={m.id}
                  className={cn("flex flex-col gap-1.5", m.role === "user" ? "items-end" : "items-start")}
                >
                  <div
                    className={cn(
                      "max-w-[92%] whitespace-pre-wrap break-words rounded-xl px-3 py-2 text-sm leading-6",
                      m.role === "user"
                        ? "bg-accent text-accent-foreground"
                        : "bg-surface-muted text-foreground",
                      m.kind === "error" && "bg-danger/10 text-danger",
                    )}
                  >
                    {m.notice !== undefined && m.pending ? (
                      <span className="mb-1 block text-xs text-muted-foreground">
                        {m.notice}
                      </span>
                    ) : null}
                    {m.text !== "" ? (
                      m.kind === "error" ? (
                        m.text
                      ) : (
                        <MarkdownLite text={m.text} onCite={onJump} />
                      )
                    ) : m.pending ? (
                      <span className="flex items-center gap-2 text-muted-foreground">
                        <Spinner size="sm" label="思考中" />
                        {m.thinking !== undefined && m.thinking > 0
                          ? `思考中… ${m.thinking} 字`
                          : "思考中…"}
                      </span>
                    ) : (
                      <span className="text-muted-foreground">(无内容)</span>
                    )}
                    {m.pending && m.text !== "" ? (
                      <span className="ml-1 inline-block h-4 w-1.5 animate-pulse bg-current align-middle" />
                    ) : null}
                  </div>
                  {m.snippets.length > 0 ? (
                    <div className="flex flex-wrap gap-1">
                      {m.snippets.map((s) => (
                        <button
                          key={`${m.id}-${s.index}-${s.title}`}
                          type="button"
                          onClick={() => onJump(s.title)}
                          className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring"
                          title="跳到该章"
                        >
                          <Badge size="sm" variant="muted">
                            {s.title}
                          </Badge>
                        </button>
                      ))}
                    </div>
                  ) : null}
                  {m.truncated === true ? (
                    <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
                      <span>回答达到输出上限被截断</span>
                      <Button
                        size="sm"
                        variant="secondary"
                        loading={streaming}
                        onClick={() => send("请从断点继续写, 不要重复已写过的内容。")}
                      >
                        继续生成
                      </Button>
                    </div>
                  ) : null}
                </div>
              ))
            )}
          </div>

          <div className="ui-safe-b shrink-0 border-t border-border/70 p-2">
            <Textarea
              autoSize
              rows={1}
              value={input}
              placeholder="问情节、人物、物品… (Enter 发送)"
              aria-label="向 AI 提问"
              className="max-h-32"
              onChange={(event) => setInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  send();
                }
              }}
            />
            <div className="mt-2 flex items-center justify-between gap-2">
              <div className="flex min-w-0 items-center gap-2">
                <Select
                  value={scope}
                  onValueChange={(value) => {
                    const next: AiScope = value === "book" ? "book" : "current";
                    setScope(next);
                    try {
                      localStorage.setItem(SCOPE_KEY, next);
                    } catch {
                      // 存不下就算了, 只是下次不记住
                    }
                  }}
                >
                  <SelectTrigger
                    size="sm"
                    aria-label="检索范围"
                    className="w-auto shrink-0 whitespace-nowrap"
                  >
                    <SelectValue placeholder="范围" />
                  </SelectTrigger>
                  <SelectContent>
                    {SCOPE_OPTIONS.map((option) => (
                      <SelectItem key={option.id} value={option.id}>
                        {option.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {/* 说明文字允许折到第二行: 手机窄面板下截断会看不到关键区别 */}
                <span className="min-w-0 flex-1 text-xs leading-snug text-muted-foreground">
                  {SCOPE_OPTIONS.find((option) => option.id === scope)?.hint ?? "基于已缓存章节"}
                </span>
              </div>
              {streaming ? (
                <Button size="sm" variant="secondary" onClick={stop}>
                  <Square aria-hidden />
                  停止
                </Button>
              ) : (
                <Button size="sm" disabled={input.trim() === ""} onClick={() => send()}>
                  发送
                  <Send aria-hidden />
                </Button>
              )}
            </div>
          </div>
        </>
      )}
    </aside>
  );
}
