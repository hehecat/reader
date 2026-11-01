import {
  CircleAlert,
  Download,
  Pencil,
  Plus,
  RotateCw,
  Search,
  Sparkles,
  Trash2,
  Upload,
} from "lucide-react";
import * as React from "react";

import { RuleEditDialog } from "@/components/purify/RuleEditDialog";
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  IconButton,
  Input,
  PageIntro,
  SkeletonList,
  Switch,
  cn,
} from "@/components/ui";
import {
  exportRulesJson,
  nextRuleOrder,
  parseImportedRules,
  purifyErrorMessage,
  useDeleteAllReplaceRules,
  useDeleteReplaceRule,
  useReplaceRules,
  useSaveReplaceRulesBulk,
  useToggleReplaceRule,
  type ImportedRules,
  type ReplaceRule,
} from "@/services/purify";
import { toast } from "@/components/ui/Toast";

/** 关键词过滤: 不区分大小写, 匹配名称/查找内容/替换内容/分组 */
function filterRules(rules: ReplaceRule[], keyword: string): ReplaceRule[] {
  const needle = keyword.trim().toLowerCase();
  if (needle.length === 0) return rules;
  return rules.filter((rule) =>
    [rule.name, rule.find, rule.replace, rule.group ?? ""].some((field) =>
      field.toLowerCase().includes(needle),
    ),
  );
}

export interface RuleRowProps {
  rule: ReplaceRule;
  /** 本行启用开关的忙碌态(切换请求进行中) */
  busy: boolean;
  onToggleEnabled: (rule: ReplaceRule, enabled: boolean) => void;
  onEdit: (rule: ReplaceRule) => void;
  onDelete: (rule: ReplaceRule) => void;
}

/** 规则列表行(父级 ul 提供 divide-y 分隔): 名称 + 正则/范围徽标 + 查找→替换预览 + 编辑/删除/启用 */
function RuleRow({ rule, busy, onToggleEnabled, onEdit, onDelete }: RuleRowProps) {
  const name = rule.name.trim().length > 0 ? rule.name : "(未命名规则)";

  return (
    <li className="flex items-center gap-3 px-4 py-3 transition-colors duration-150 ease-out hover:bg-surface-muted/60">
      <span
        aria-hidden
        className={cn(
          "hidden size-10 shrink-0 items-center justify-center rounded-lg sm:flex",
          rule.enabled ? "bg-accent/10 text-accent" : "bg-surface-muted text-muted-foreground",
        )}
      >
        <Sparkles className="size-5" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <span
            className={cn("truncate text-sm font-medium", !rule.enabled && "text-muted-foreground")}
            title={name}
          >
            {name}
          </span>
          {/* 匹配方式是中性元信息: secondary 实底, 不靠颜色区分正则/字面量 */}
          {rule.isRegex ? (
            <Badge size="sm" variant="muted" className="shrink-0 bg-secondary text-foreground">
              正则
            </Badge>
          ) : null}
          {rule.scopeContent ? (
            <Badge size="sm" variant="outline" className="shrink-0">
              正文
            </Badge>
          ) : null}
          {rule.scopeTitle ? (
            <Badge size="sm" variant="outline" className="shrink-0">
              标题
            </Badge>
          ) : null}
        </div>
        <p className="mt-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          <span className="min-w-0 truncate font-mono" title={rule.find}>
            {rule.find}
          </span>
          <span aria-hidden className="shrink-0">
            →
          </span>
          <span
            className="min-w-0 shrink-0 truncate font-mono max-w-40"
            title={rule.replace === "" ? "(删除匹配到的文字)" : rule.replace}
          >
            {rule.replace === "" ? "(删除)" : rule.replace}
          </span>
        </p>
      </div>

      <div className="flex shrink-0 items-center gap-0.5">
        <IconButton size="sm" variant="ghost" tooltip="编辑" onClick={() => onEdit(rule)}>
          <Pencil aria-hidden />
        </IconButton>
        <IconButton
          size="sm"
          variant="ghost"
          tooltip="删除"
          className="text-muted-foreground hover:bg-danger/10 hover:text-danger"
          onClick={() => onDelete(rule)}
        >
          <Trash2 aria-hidden />
        </IconButton>
        <Switch
          size="sm"
          className="ml-1"
          checked={rule.enabled}
          disabled={busy}
          aria-label={`${rule.enabled ? "停用" : "启用"} ${name}`}
          onCheckedChange={(enabled) => onToggleEnabled(rule, enabled)}
        />
      </div>
    </li>
  );
}

/**
 * 正文净化页: 规则列表 + 新增/编辑弹窗 + 删除确认.
 * warp 只存规则不应用, 生效发生在阅读器渲染期(useChapterContent),
 * 因此这里改完规则, 正在读的章节会立即重渲染.
 */
export default function PurifyPage() {
  const { rules, isLoading, isFetching, error, refetch } = useReplaceRules();
  const toggleEnabled = useToggleReplaceRule();
  const deleteRule = useDeleteReplaceRule();

  const [keyword, setKeyword] = React.useState("");
  const [editOpen, setEditOpen] = React.useState(false);
  const [editRule, setEditRule] = React.useState<ReplaceRule | null>(null);
  const [pendingDelete, setPendingDelete] = React.useState<ReplaceRule | null>(null);
  const [importOpen, setImportOpen] = React.useState(false);
  const [importText, setImportText] = React.useState("");
  const [importMode, setImportMode] = React.useState<"merge" | "overwrite">("merge");
  const saveBulk = useSaveReplaceRulesBulk();
  const deleteAll = useDeleteAllReplaceRules();
  const fileRef = React.useRef<HTMLInputElement>(null);

  const visibleRules = React.useMemo(() => filterRules(rules, keyword), [rules, keyword]);
  // 只有正在切换的那一行开关进入忙碌态
  const busyId = toggleEnabled.isPending ? toggleEnabled.variables?.id : undefined;
  const enabledCount = rules.filter((rule) => rule.enabled).length;

  const openCreate = (): void => {
    setEditRule(null);
    setEditOpen(true);
  };

  const confirmDelete = (): void => {
    if (pendingDelete === null) return;
    deleteRule.mutate(pendingDelete.id, { onSuccess: () => setPendingDelete(null) });
  };

  const parsedImport = React.useMemo((): { ok: ImportedRules | null; error: string | null } => {
    if (importText.trim() === "") return { ok: null, error: null };
    try {
      return { ok: parseImportedRules(importText, nextRuleOrder(rules)), error: null };
    } catch (error) {
      return { ok: null, error: error instanceof Error ? error.message : "JSON 解析失败" };
    }
  }, [importText, rules]);

  const handleExport = (): void => {
    const blob = new Blob([exportRulesJson(rules)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `purify-rules-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(url);
    toast.success(`已导出 ${rules.length} 条规则`);
  };

  const handleImportFile = (file: File): void => {
    void file.text().then((text) => setImportText(text));
  };

  const confirmImport = (): void => {
    if (parsedImport?.ok == null || parsedImport.ok.rules.length === 0) return;
    const incoming = parsedImport.ok.rules;
    const commit = (payload: ReplaceRule[]): void => {
      saveBulk.mutate(payload, {
        onSuccess: () => {
          toast.success(`已导入 ${payload.length} 条规则`);
          setImportOpen(false);
          setImportText("");
        },
        onError: (error) => toast.error(purifyErrorMessage(error, "导入失败")),
      });
    };
    if (importMode === "overwrite") {
      deleteAll.mutate(undefined, { onSuccess: () => commit(incoming) });
      return;
    }
    const existing = new Set(rules.map((rule) => `${rule.name}\u0000${rule.find}`));
    const merged = incoming.filter((rule) => !existing.has(`${rule.name}\u0000${rule.find}`));
    if (merged.length === 0) {
      toast.error("没有可导入的规则(全部与现有规则重名且查找内容相同)");
      return;
    }
    commit(merged);
  };
  return (
    <div className="mx-auto flex min-h-full w-full max-w-4xl flex-col px-4 pb-10 pt-5 sm:px-6 md:px-10 md:pt-8">
      <PageIntro
        eyebrow="TEXT PURIFY"
        title="正文净化"
        desc="替换规则在正文渲染前生效: 抹掉章尾求票、统一标点、删掉重复段落. 规则存在服务端, 换设备也在."
        action={
          <Button size="sm" onClick={openCreate}>
            <Plus aria-hidden />
            新增规则
          </Button>
        }
      />

      <div className="mb-5 flex flex-wrap items-center gap-2">
        <Button size="sm" variant="secondary" disabled={isLoading || isFetching} onClick={refetch}>
          <RotateCw aria-hidden className={cn(isFetching && "ui-spin")} />
          刷新规则
        </Button>
        <Button size="sm" variant="secondary" disabled={rules.length === 0} onClick={handleExport}>
          <Download aria-hidden />
          导出规则
        </Button>
        <Button
          size="sm"
          variant="secondary"
          onClick={() => {
            setImportText("");
            setImportMode("merge");
            setImportOpen(true);
          }}
        >
          <Upload aria-hidden />
          导入规则
        </Button>
        {!isLoading && error === null && rules.length > 0 ? (
          <span className="ml-auto text-xs text-muted-foreground tabular-nums">
            {rules.length} 条规则 · {enabledCount} 条启用
          </span>
        ) : null}
      </div>

      <div className="rounded-xl border border-border bg-surface p-4">
        <Input
          aria-label="在规则中筛选"
          placeholder="搜索名称 / 查找内容"
          className="mb-3 max-w-full"
          value={keyword}
          onChange={(event) => setKeyword(event.target.value)}
          clearable
          onClear={() => setKeyword("")}
          prefixIcon={<Search aria-hidden className="size-4 text-muted-foreground" />}
        />
        {isLoading ? (
          <SkeletonList count={4} className="py-1" />
        ) : error !== null ? (
          <EmptyState
            compact
            icon={<CircleAlert aria-hidden />}
            title="净化规则加载失败"
            description={purifyErrorMessage(error, "网络异常或登录态已失效")}
            action={
              <Button size="sm" variant="secondary" onClick={refetch}>
                <RotateCw aria-hidden />
                重试
              </Button>
            }
            className="rounded-xl border border-dashed border-border py-10"
          />
        ) : rules.length === 0 ? (
          <EmptyState
            compact
            icon={<Sparkles aria-hidden />}
            title="还没有净化规则"
            description="在阅读器里划选一段文字, 工具条上点「添加净化」就能一键生成整句规则; 也可以在这里手动新增."
            action={
              <Button size="sm" onClick={openCreate}>
                <Plus aria-hidden />
                新增规则
              </Button>
            }
            className="rounded-xl border border-dashed border-border py-10"
          />
        ) : visibleRules.length === 0 ? (
          <EmptyState
            compact
            icon={<Search aria-hidden />}
            title="没有符合条件的规则"
            description="换个关键词试试"
            action={
              <Button size="sm" variant="secondary" onClick={() => setKeyword("")}>
                清除筛选
              </Button>
            }
            className="rounded-xl border border-dashed border-border py-10"
          />
        ) : (
          <ul className="divide-y divide-border/70">
            {visibleRules.map((rule) => (
              <RuleRow
                key={rule.id}
                rule={rule}
                busy={busyId === rule.id}
                onToggleEnabled={(target, enabled) =>
                  toggleEnabled.mutate({ id: target.id, enabled })
                }
                onEdit={(target) => {
                  setEditRule(target);
                  setEditOpen(true);
                }}
                onDelete={setPendingDelete}
              />
            ))}
          </ul>
        )}
      </div>

      <RuleEditDialog rule={editRule} open={editOpen} onOpenChange={setEditOpen} />

      <Dialog
        open={pendingDelete !== null}
        onOpenChange={(open) => {
          if (!open) setPendingDelete(null);
        }}
      >
        <DialogContent width="sm">
          <DialogHeader>
            <DialogTitle>删除净化规则</DialogTitle>
            <DialogDescription>
              确定删除「{pendingDelete?.name ?? ""}」吗? 删除后正文会立刻恢复成原文.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setPendingDelete(null)}>
              取消
            </Button>
            <Button variant="danger" loading={deleteRule.isPending} onClick={confirmDelete}>
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={importOpen} onOpenChange={setImportOpen}>
        <DialogContent width="md">
          <DialogHeader>
            <DialogTitle>导入净化规则</DialogTitle>
            <DialogDescription>
              支持本端导出格式与阅读 legacy 导出 ({`{regex, replacement, replaceSummary…}`}); 粘贴 JSON 或选择文件.
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col gap-3 px-4 md:px-5">
            <div className="flex items-center gap-2">
              <Button size="sm" variant="secondary" onClick={() => fileRef.current?.click()}>
                选择 JSON 文件
              </Button>
              <input
                ref={fileRef}
                type="file"
                accept=".json,application/json"
                className="hidden"
                onChange={(event) => {
                  const file = event.target.files?.[0];
                  if (file) handleImportFile(file);
                  event.target.value = "";
                }}
              />
              <label className="ml-auto flex items-center gap-1.5 text-xs text-muted-foreground">
                <input
                  type="radio"
                  name="import-mode"
                  checked={importMode === "merge"}
                  onChange={() => setImportMode("merge")}
                />
                合并(跳过同名同查找)
              </label>
              <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <input
                  type="radio"
                  name="import-mode"
                  checked={importMode === "overwrite"}
                  onChange={() => setImportMode("overwrite")}
                />
                覆盖(清空后导入)
              </label>
            </div>
            <textarea
              aria-label="导入 JSON 内容"
              className="min-h-40 w-full rounded-xl border border-border bg-surface-muted/40 p-3 font-mono text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
              placeholder='[{"name":"杭城","find":"杭城","replace":"杭州","isRegex":false,…}]'
              value={importText}
              onChange={(event) => setImportText(event.target.value)}
            />
            {parsedImport !== null ? (
              parsedImport.ok !== null ? (
                <p className="text-xs text-muted-foreground">
                  解析出 {parsedImport.ok.rules.length} 条规则
                  {parsedImport.ok.skipped.length > 0
                    ? `, 跳过 ${parsedImport.ok.skipped.length} 条 (${parsedImport.ok.skipped.slice(0, 3).join("、")}${parsedImport.ok.skipped.length > 3 ? "…" : ""})`
                    : ""}
                </p>
              ) : parsedImport.error !== null ? (
                <p className="text-xs text-danger">解析失败: {parsedImport.error}</p>
              ) : null
            ) : null}
          </div>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setImportOpen(false)}>
              取消
            </Button>
            <Button
              loading={saveBulk.isPending || deleteAll.isPending}
              disabled={parsedImport?.ok == null || parsedImport.ok.rules.length === 0}
              onClick={confirmImport}
            >
              导入
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
