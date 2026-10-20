import * as React from "react";

/**
 * 轻量 Markdown 渲染（AI 回答用）：段落 / 列表 / 标题 / 引用 / 行内粗体与代码，
 * 并把 `[第N章 标题]` 变成可点击的章节引用。不引入 markdown 依赖，够用即可。
 */

/** 行内：**粗体** / `代码` / [第N章 xxx] 引用 */
function renderInline(
  text: string,
  onCite: ((title: string) => void) | undefined,
  keyPrefix: string,
): React.ReactNode[] {
  const nodes: React.ReactNode[] = [];
  const re = /(\*\*[^*]+\*\*|`[^`]+`|\[[^\[\]]*?第[^\[\]]*?章[^\[\]]*?\])/g;
  let last = 0;
  let seq = 0;
  let match: RegExpExecArray | null = re.exec(text);
  while (match !== null) {
    if (match.index > last) {
      nodes.push(text.slice(last, match.index));
    }
    const token = match[0];
    const key = `${keyPrefix}-${seq++}`;
    if (token.startsWith("**")) {
      nodes.push(
        <strong key={key} className="font-semibold text-foreground">
          {token.slice(2, -2)}
        </strong>,
      );
    } else if (token.startsWith("`")) {
      nodes.push(
        <code key={key} className="rounded bg-surface px-1 py-0.5 font-mono text-[0.9em]">
          {token.slice(1, -1)}
        </code>,
      );
    } else {
      const title = token.slice(1, -1).trim();
      nodes.push(
        onCite === undefined ? (
          <span key={key} className="text-accent">
            {title}
          </span>
        ) : (
          <button
            key={key}
            type="button"
            title="跳到该章"
            onClick={() => onCite(title)}
            className="mx-0.5 inline-flex items-baseline rounded bg-accent/10 px-1 text-[0.92em] text-accent outline-none hover:bg-accent/15 focus-visible:ring-2 focus-visible:ring-ring"
          >
            {title}
          </button>
        ),
      );
    }
    last = match.index + token.length;
    match = re.exec(text);
  }
  if (last < text.length) {
    nodes.push(text.slice(last));
  }
  return nodes;
}

export function MarkdownLite({
  text,
  onCite,
}: {
  text: string;
  onCite?: (title: string) => void;
}) {
  const blocks = React.useMemo(() => {
    const out: React.ReactNode[] = [];
    let list: { ordered: boolean; items: string[] } | null = null;
    const flush = (): void => {
      if (list === null) {
        return;
      }
      const items = list.items;
      const key = `list-${out.length}`;
      out.push(
        list.ordered ? (
          <ol key={key} className="flex list-decimal flex-col gap-1 pl-5">
            {items.map((item, i) => (
              <li key={i}>{renderInline(item, onCite, `${key}-${i}`)}</li>
            ))}
          </ol>
        ) : (
          <ul key={key} className="flex list-disc flex-col gap-1 pl-5">
            {items.map((item, i) => (
              <li key={i}>{renderInline(item, onCite, `${key}-${i}`)}</li>
            ))}
          </ul>
        ),
      );
      list = null;
    };

    for (const raw of text.split("\n")) {
      const line = raw.replace(/\s+$/, "");
      if (line.trim() === "") {
        flush();
        continue;
      }
      const heading = /^(#{1,4})\s+(.*)$/.exec(line);
      if (heading !== null) {
        flush();
        out.push(
          <p key={`h-${out.length}`} className="mt-1 font-semibold text-foreground">
            {renderInline(heading[2] ?? "", onCite, `h-${out.length}`)}
          </p>,
        );
        continue;
      }
      const ul = /^\s*[-*+]\s+(.*)$/.exec(line);
      if (ul !== null) {
        if (list === null || list.ordered) {
          flush();
          list = { ordered: false, items: [] };
        }
        list.items.push(ul[1] ?? "");
        continue;
      }
      const ol = /^\s*(\d+)[.)]\s+(.*)$/.exec(line);
      if (ol !== null) {
        if (list === null || !list.ordered) {
          flush();
          list = { ordered: true, items: [] };
        }
        list.items.push(ol[2] ?? "");
        continue;
      }
      const quote = /^>\s?(.*)$/.exec(line);
      if (quote !== null) {
        flush();
        out.push(
          <blockquote
            key={`q-${out.length}`}
            className="border-l-2 border-border pl-2 text-muted-foreground"
          >
            {renderInline(quote[1] ?? "", onCite, `q-${out.length}`)}
          </blockquote>,
        );
        continue;
      }
      flush();
      out.push(
        <p key={`p-${out.length}`} className="whitespace-pre-wrap">
          {renderInline(line, onCite, `p-${out.length}`)}
        </p>,
      );
    }
    flush();
    return out;
  }, [text, onCite]);

  return <div className="flex flex-col gap-2 text-sm leading-6">{blocks}</div>;
}
