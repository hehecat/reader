import { CircleCheck, PlugZap } from "lucide-react";
import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Input, SettingCard, SettingRow, toast } from "@/components/ui";
import { humanizeError } from "@/lib/errors";
import {
  getJevSettings,
  saveJevSettings,
  testJevEndpoint,
  type JevSettingsInput,
} from "@/services/ai";

const DEFAULT_MODEL = "typesafe/jev-latest";

/** 把 `/decisions` 的 answers 压成一行人话, 例如 "dead: 否 (0.02) · who: zhaojiu" */
function summarizeAnswers(answers: unknown): string {
  if (answers === null || typeof answers !== "object") {
    return "端点已响应";
  }
  const parts: string[] = [];
  for (const [key, value] of Object.entries(answers as Record<string, unknown>)) {
    if (value === null || typeof value !== "object") {
      parts.push(`${key}: ${String(value)}`);
      continue;
    }
    const entry = value as Record<string, unknown>;
    const noul = entry.noul;
    if (typeof noul === "number") {
      parts.push(`${key}: ${noul >= 0.5 ? "是" : "否"} (${noul.toFixed(2)})`);
      continue;
    }
    const choice = entry.choice ?? entry.value ?? entry.label;
    if (choice !== undefined && choice !== null) {
      parts.push(`${key}: ${String(choice)}`);
      continue;
    }
    parts.push(`${key}: ${JSON.stringify(entry).slice(0, 40)}`);
  }
  return parts.length === 0 ? "端点已响应" : parts.join(" · ");
}

interface JevSettingsFormProps {
  baseUrl: string;
  model: string;
  keySet: boolean;
  enabled: boolean;
}

/** 表单独立成组件: 用 key 重置草稿, 避免在 effect 里 setState */
function JevSettingsForm({ baseUrl, model, keySet, enabled }: JevSettingsFormProps) {
  const queryClient = useQueryClient();
  const [draftBaseUrl, setDraftBaseUrl] = React.useState(baseUrl);
  const [apiKey, setApiKey] = React.useState("");
  const [draftModel, setDraftModel] = React.useState(model);
  const [testResult, setTestResult] = React.useState<string | null>(null);

  const input: JevSettingsInput = {
    baseUrl: draftBaseUrl,
    apiKey,
    model: draftModel,
  };

  const save = useMutation({
    mutationFn: () => saveJevSettings(input),
    onSuccess: () => {
      setApiKey("");
      setTestResult(null);
      toast.success("已保存, 立即生效");
      void queryClient.invalidateQueries({ queryKey: ["jevSettings"] });
    },
    onError: (error) => {
      toast.error(humanizeError(error instanceof Error ? error.message : String(error)));
    },
  });

  const test = useMutation({
    mutationFn: () => testJevEndpoint(input),
    onSuccess: (result) => {
      setTestResult(
        `判定正常 · ${result.latencyMs ?? 0} ms · ${summarizeAnswers(result.answers)}`,
      );
    },
    onError: (error) => {
      setTestResult(null);
      toast.error(humanizeError(error instanceof Error ? error.message : String(error)));
    },
  });

  return (
    <>
      <SettingRow label="状态" value={enabled ? "已配置" : "未配置"}>
        <span className="text-xs text-muted-foreground">
          {enabled ? model || "已启用" : "未配置时相关功能自动跳过"}
        </span>
      </SettingRow>
      <SettingRow
        label="端点地址"
        value="服务根地址(不含 /decisions), 例如 https://emrank.tech/v1"
      >
        <Input
          className="w-full sm:w-72"
          value={draftBaseUrl}
          placeholder="https://emrank.tech/v1"
          onChange={(event) => setDraftBaseUrl(event.target.value)}
        />
      </SettingRow>
      <SettingRow
        label="API Key"
        value={keySet ? "已设置, 留空保持不变" : "未设置"}
      >
        <Input
          className="w-full sm:w-72"
          type="password"
          value={apiKey}
          placeholder={keySet ? "••••••(留空保持)" : "sk-..."}
          autoComplete="off"
          onChange={(event) => setApiKey(event.target.value)}
        />
      </SettingRow>
      <SettingRow label="模型" value={`例如 ${DEFAULT_MODEL}`}>
        <Input
          className="w-full sm:w-72"
          value={draftModel}
          placeholder={DEFAULT_MODEL}
          onChange={(event) => setDraftModel(event.target.value)}
        />
      </SettingRow>
      <SettingRow
        label="连通性"
        value={testResult ?? "测试会真发一次判定(一个是否题 + 一个选择题)"}
      >
        <div className="flex items-center gap-2">
          <Button
            size="sm"
            variant="secondary"
            loading={test.isPending}
            onClick={() => test.mutate()}
          >
            <PlugZap aria-hidden />
            测试
          </Button>
          <Button size="sm" loading={save.isPending} onClick={() => save.mutate()}>
            <CircleCheck aria-hidden />
            保存
          </Button>
        </div>
      </SettingRow>
    </>
  );
}

/**
 * 设置页「判定模型（Jev / EMRank）」卡：
 * - 与阅读助手的 chat 端点不是一回事: 一次提交「状态 + 一组封闭式问题」, 走 `POST {base}/decisions`；
 * - 端点 / 密钥 / 模型单独存（system_settings.jev_settings），保存即生效；
 * - 「测试连接」真发一次 noul + choice 判定，回显答案与延迟，用来确认协议与权限都对。
 */
export function JevSettingsCard() {
  const settingsQuery = useQuery({
    queryKey: ["jevSettings"],
    queryFn: getJevSettings,
    staleTime: 30_000,
  });
  const data = settingsQuery.data;
  const baseUrl = data?.baseUrl ?? "";
  const model = data?.model ?? DEFAULT_MODEL;
  const keySet = data?.keySet === true;

  return (
    <SettingCard
      title="判定模型 (Jev)"
      desc="封闭式问答/判定用的独立端点(EMRank); 与上面 AI 助手的 chat 端点不同"
    >
      <JevSettingsForm
        key={`${baseUrl}|${model}|${String(keySet)}`}
        baseUrl={baseUrl}
        model={model}
        keySet={keySet}
        enabled={data?.enabled === true}
      />
    </SettingCard>
  );
}
