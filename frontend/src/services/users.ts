/**
 * 用户管理接口封装(仅管理员). 全部走管理密码(secureKey)第二因子:
 * 后端多用户模式下要求「已登录 + is_admin」, 配置了 secureKey 时再校验该密码;
 * 单用户模式(非 secure)无登录概念, 配置了 secureKey 则按密码校验.
 */
import { z } from "zod";
import {
  get,
  parseWith,
  post,
  type ApiParams,
  type ApiRequestConfig,
} from "@/lib/api-client";
import { getSecureKey } from "@/lib/storage";

export const adminUserSchema = z.object({
  username: z.string(),
  isAdmin: z.boolean().default(false),
  disabled: z.boolean().default(false),
  enableWebdav: z.boolean().default(false),
  enableLocalStore: z.boolean().default(false),
  enableBookSource: z.boolean().default(false),
  enableRssSource: z.boolean().default(false),
  bookSourceLimit: z.number().default(0),
  bookLimit: z.number().default(0),
  lastLoginAt: z.number().default(0),
  createdAt: z.number().default(0),
});

export type AdminUser = z.infer<typeof adminUserSchema>;

const adminUserListSchema = z.array(adminUserSchema);

/** 管理密码参数(未显式传入时取本地保存的); GET 走 params, POST 走 config.params */
function secureKeyParams(secureKey?: string): ApiParams {
  const key = secureKey ?? getSecureKey();
  return key === null ? {} : { secureKey: key };
}

/** 管理请求配置: POST 用 config.params 携带 secureKey */
function managerConfig(secureKey?: string): ApiRequestConfig {
  return { params: secureKeyParams(secureKey) };
}

/** 用户列表(含权限/配额/最后登录/创建时间) */
export async function listUsers(secureKey?: string): Promise<AdminUser[]> {
  const data = await get<unknown>("/getUsers", secureKeyParams(secureKey));
  return parseWith(adminUserListSchema, data);
}

export interface AddUserInput {
  username: string;
  password: string;
  isAdmin?: boolean;
  enableWebdav?: boolean;
  enableLocalStore?: boolean;
  enableBookSource?: boolean;
  enableRssSource?: boolean;
  bookSourceLimit?: number;
  bookLimit?: number;
}

export async function addUser(input: AddUserInput, secureKey?: string): Promise<void> {
  await post<unknown>("/addUser", input, managerConfig(secureKey));
}

export interface UpdateUserInput {
  username: string;
  isAdmin?: boolean;
  disabled?: boolean;
  enableWebdav?: boolean;
  enableLocalStore?: boolean;
  enableBookSource?: boolean;
  enableRssSource?: boolean;
  bookSourceLimit?: number;
  bookLimit?: number;
}

export async function updateUser(input: UpdateUserInput, secureKey?: string): Promise<void> {
  await post<unknown>("/updateUser", input, managerConfig(secureKey));
}

export async function deleteUser(username: string, secureKey?: string): Promise<void> {
  await post<unknown>("/deleteUser", { username }, managerConfig(secureKey));
}

export async function resetUserPassword(
  username: string,
  password: string,
  secureKey?: string,
): Promise<void> {
  await post<unknown>("/resetUserPassword", { username, password }, managerConfig(secureKey));
}

/** 清理不活跃用户(按天数); 返回清理数量(后端字段名以 count 为准) */
export async function clearInactiveUsers(days: number, secureKey?: string): Promise<number> {
  const data = await post<unknown>("/clearInactiveUsers", { day: days }, managerConfig(secureKey));
  const parsed = z.object({ count: z.number().default(0) }).safeParse(data);
  return parsed.success ? parsed.data.count : 0;
}
