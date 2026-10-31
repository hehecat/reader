/**
 * 源→该书在该源的最新章标题(源站原文, 可能是「已完结」这类状态词)。
 *
 * 搜索聚合会把同名同作者的多源命中并成一本书, 但 `latestChapterTitle` 只有一个字段
 * (代表源的那个)。换源面板要按源显示各自的最新章, 所以聚合时把这层映射单独留着 ——
 * 否则面板里所有行都会显示代表源的状态词, 过滤掉状态词后整列空白。
 */
export type OriginLatest = Record<string, string>;

/**
 * 把一条命中并进映射: 同一源重复命中时以最新一次为准, 空标题不占位。
 * `extra` 用于合并后端已带回的映射。
 */
export function mergeOriginLatest(
  previous: OriginLatest | undefined,
  origin: string,
  title: string | undefined,
  extra?: OriginLatest,
): OriginLatest {
  return {
    ...(previous ?? {}),
    ...(extra ?? {}),
    ...(title !== undefined && title.trim().length > 0 ? { [origin]: title } : {}),
  };
}
