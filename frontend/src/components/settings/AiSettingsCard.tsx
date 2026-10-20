import { CircleCheck, PlugZap } from "lucide-react";
import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Input, SettingCard, SettingRow, toast } from "@/components/ui";
import { humanizeError } from "@/lib/errors";
import {
  getAiSettings,
  saveAiSettings,
  testAiEndpoint,
  type AiSettingsInput,
} from "@/services/ai";

/**
 * 设置页「AI 阅读助手」卡：
 * - 端点 / 密钥 / 模型 / max tokens 存服务端（system_settings），保存即生效，无需重启容器；
 * - 测试按钮用当前输入（或已保存配置）探测 `/models`；
 * - 密钥留空 = 保留已保存的密钥；端点为"环境变量"时说明来自 READER_AI_*，保存后以页面配置为准。
 */
export function AiSettingsCard() {
  const queryClient = useQueryClient();
  const settingsQuery = useQuery({
    queryKey: ["aiSettings"],
    queryFn: getAiSettings,
    staleTime: 30_000,
  });
  const data = settingsQuery.data;

  const [baseUrl, setBaseUrl] = React.useState("");
  const [apiKey, setApiKey] = React.useState("");
  const [model, setModel] = React.useState("deepseek-v4.1-flash");
  const [maxTokens, setMaxTokens] = React.useState(3500);
  const [testResult, setTestResult] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (data === undefined) {
      return;
    }
    setBaseUrl(data.baseUrl);
    setModel(data.model || "deepseek-v4.1-flash");
    setMaxTokens(data.maxTokens || 3500);
    setApiKey("");
  }, [data]);

  const input: AiSettingsInput = { baseUrl, apiKey, model, maxTokens };

  const save = useMutation({
    mutationFn: () => saveAiSettings(input),
    onSuccess: () => {
      setApiKey("");
      setTestResult(null);
      toast.success("已保存, 立即生效");
      void queryClient.invalidateQueries({ queryKey: ["aiSettings"] });
      void queryClient.invalidateQueries({ queryKey: ["aiStatus"] });
    },
    onError: (error) => {
      toast.error(humanizeError(error instanceof Error ? error.message : String(error)));
    },
  });

  const test = useMutation({
    mutationFn: () => testAiEndpoint(input),
    onSuccess: (result) => {
      setTestResult(
        `连接正常 · ${result.models ?? 0} 个模型 · ${result.latencyMs ?? 0} ms`,
      );
    },
    onError: (error) => {
      setTestResult(null);
      toast.error(humanizeError(error instanceof Error ? error.message : String(error)));
    },
  });

  const sourceLabel =
    data?.source === "saved"
      ? "设置页已保存"
      : data?.source === "env"
        ? "环境变量 (READER_AI_*)"
        : "未配置";

  return (
    <SettingCard
      title="AI 阅读助手"
      desc="阅读器右侧问答的模型端点; 保存即生效, 无需重启容器"
    >
      <SettingRow label="状态" value={`来源: ${sourceLabel}`}>
        <span className="text-xs text-muted-foreground">
          {data?.enabled === true ? data.model || "已启用" : "未配置"}
        </span>
      </SettingRow>
      <SettingRow label="端点地址" value="OpenAI 兼容接口根地址, 例如 http://octopus:8080/v1">
        <Input
          className="w-full sm:w-72"
          value={baseUrl}
          placeholder="http://octopus:8080/v1"
          onChange={(event) => setBaseUrl(event.target.value)}
        />
      </SettingRow>
      <SettingRow
        label="API Key"
        value={data?.keySet === true ? "已设置, 留空保持不变" : "未设置"}
      >
        <Input
          className="w-full sm:w-72"
          type="password"
          value={apiKey}
          placeholder={data?.keySet === true ? "••••••(留空保持)" : "sk-..."}
          autoComplete="off"
          onChange={(event) => setApiKey(event.target.value)}
        />
      </SettingRow>
      <SettingRow label="模型" value="支持推理模型 (思考过程不展示给读者)">
        <Input
          className="w-full sm:w-72"
          value={model}
          placeholder="deepseek-v4.1-flash"
          onChange={(event) => setModel(event.target.value)}
        />
      </SettingRow>
      <SettingRow label="最大输出 tokens" value="200-8000; 推理模型建议 4000+">
        <input
          type="number"
          min={200}
          max={8000}
          step={100}
          value={maxTokens}
          onChange={(event) => setMaxTokens(Number(event.target.value))}
          className="w-24 rounded-md border border-border bg-background px-3 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        />
      </SettingRow>
      <SettingRow label="连通性" value={testResult ?? "填好端点后先点测试"}>
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
    </SettingCard>
  );
}
