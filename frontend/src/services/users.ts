/**
 * 用户管理接口封装(仅管理员): 后端只校验「已登录 + is_admin」, 无管理密码.
 */
import { z } from "zod";
import { get, parseWith, post } from "@/lib/api-client";

export const adminUserSchema = z.object({
  username: z.string(),
  isAdmin: z.boolean().default(false),
  disabled: z.boolean().default(false),
  enableWebdav: z.boolean().default(false),
  enableLocalStore: z.boolean().default(false),
  enableBookSource: z.boolean().default(false),
  enableRssSource: z.boolean().default(false),
  /** 当前该用户的书源数(后端 GROUP BY 统计; 旧后端不返回时缺失) */
  bookSourceCount: z.number().optional(),
  /** 当前该用户的书籍数 */
  bookCount: z.number().optional(),
  bookSourceLimit: z.number().default(0),
  bookLimit: z.number().default(0),
  lastLoginAt: z.number().default(0),
  createdAt: z.number().default(0),
});

export type AdminUser = z.infer<typeof adminUserSchema>;

const adminUserListSchema = z.array(adminUserSchema);

/** 用户列表(含权限/配额/最后登录/创建时间) */
export async function listUsers(): Promise<AdminUser[]> {
  const data = await get<unknown>("/getUsers");
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

export async function addUser(input: AddUserInput): Promise<void> {
  await post<unknown>("/addUser", input);
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

export async function updateUser(input: UpdateUserInput): Promise<void> {
  await post<unknown>("/updateUser", input);
}

export async function deleteUser(username: string): Promise<void> {
  await post<unknown>("/deleteUser", { username });
}

export async function resetUserPassword(username: string, password: string): Promise<void> {
  await post<unknown>("/resetUserPassword", { username, password });
}

/** 清理不活跃用户(按天数); 返回清理数量(后端字段名以 count 为准) */
export async function clearInactiveUsers(days: number): Promise<number> {
  const data = await post<unknown>("/clearInactiveUsers", { day: days });
  const parsed = z.object({ count: z.number().default(0) }).safeParse(data);
  return parsed.success ? parsed.data.count : 0;
}
