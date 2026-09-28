import { z } from "zod";
import {
  get,
  parseWith,
  post,
  UnauthorizedError,
  type ApiRequestConfig,
} from "@/lib/api-client";
import { loginResultSchema, type LoginResult } from "@/types/api";

/** `/getUserInfo` 返回: 当前登录用户 + 服务端安全开关 */
export const userInfoSchema = z.object({
  /** 未登录时后端省略该字段 */
  userInfo: loginResultSchema.optional(),
  secure: z.boolean().optional(),
  /** 服务端是否配置了管理密码 */
  secureKey: z.boolean().optional(),
});

/**
 * `/getSystemInfo` 返回 (warp shape): 结构化监控聚合 + 版本.
 * 另有 port/userCount/bookCount/bookSourceCount 与 legacy 内存字符串字段
 * (freeMemory/totalMemory/maxMemory), UI 未消费, 不进 schema.
 */
export const systemInfoSchema = z.object({
  version: z.string().optional(),
  timestamp: z.number().optional(),
  /** 多用户模式(需登录); 单用户模式为 false —— 前端据此跳过登录页 */
  secure: z.boolean().optional(),
  /** 运行模式: single(单用户) | multi(多用户) */
  mode: z.enum(["single", "multi"]).optional(),
  /** 当前请求者是否管理员(未登录=false; 内存/CPU 等运维字段仅管理员可见) */
  isAdmin: z.boolean().optional(),
  /** 注册是否需要邀请码 */
  inviteRequired: z.boolean().optional(),
  userLimit: z.number().optional(),
  /** 服务已运行秒数 */
  uptimeSeconds: z.number().optional(),
  memory: z
    .object({
      totalMb: z.number().optional(),
      availableMb: z.number().optional(),
      usedMb: z.number().optional(),
      processMb: z.number().optional(),
      /** 已用百分比 (0-100, 一位小数) */
      percent: z.number().optional(),
    })
    .optional(),
  cpu: z
    .object({
      percent: z.number().optional(),
    })
    .optional(),
});

export type UserInfo = z.infer<typeof userInfoSchema>;
export type SystemInfo = z.infer<typeof systemInfoSchema>;

/**
 * 登录 / 注册.
 * @param isLogin true=登录, false=注册(用户名 ≥5 位, 密码 ≥8 位)
 * @param code 注册邀请码, 服务端配置了 inviteCode 时必填
 */
export async function login(
  username: string,
  password: string,
  isLogin: boolean,
  code?: string,
  config?: ApiRequestConfig,
): Promise<LoginResult> {
  const data = await post<unknown>("/login", { username, password, isLogin, code }, config);
  return parseWith(loginResultSchema, data);
}

/**
 * 登出. warp 成功时返回 `data: null` (仅移除当前设备 token);
 * 登录态已失效时会走 NEED_LOGIN 失败分支, 两种情况都视为已登出.
 */
export async function logout(config?: ApiRequestConfig): Promise<void> {
  try {
    await post<unknown>("/logout", undefined, config);
  } catch (error) {
    if (error instanceof UnauthorizedError) {
      return;
    }
    throw error;
  }
}

export async function getUserInfo(config?: ApiRequestConfig): Promise<UserInfo> {
  const data = await get<unknown>("/getUserInfo", undefined, config);
  return parseWith(userInfoSchema, data);
}

export async function getSystemInfo(config?: ApiRequestConfig): Promise<SystemInfo> {
  const data = await get<unknown>("/getSystemInfo", undefined, config);
  return parseWith(systemInfoSchema, data);
}
