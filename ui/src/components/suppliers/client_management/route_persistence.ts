import type { Route } from '../../../lib/api/proxy.js';

export interface Route_write_plan {
  to_save: Route[];
  to_delete: Route[];
}

export interface Route_write_readback {
  plan: Route_write_plan;
  rejected_aliases: string[];
}

export interface Route_write_receipt {
  baseline: Route[];
  saved_count: number;
  failed_aliases: string[];
  persisted_after_rejection: string[];
  pending_readback: Route_write_readback | null;
}

export interface Route_rename_readback {
  source: Route;
  target_alias: string;
}

interface Route_storage {
  save: (route: Route) => Promise<void>;
  remove: (alias: string) => Promise<void>;
  read: () => Promise<Route[]>;
}

function route_matches(left: Route | undefined, right: Route): boolean {
  return left !== undefined && left.alias === right.alias && left.description === right.description &&
    left.hops.length === right.hops.length && left.hops.every((hop, index) =>
      hop.provider === right.hops[index].provider && hop.model === right.hops[index].model);
}

/** The DAO rename is atomic. A retained source proves it did not commit;
 * the exact former row under the target proves the requested result. */
export async function read_route_rename(
  readback: Route_rename_readback,
  baseline: Route[],
  read: Route_storage['read'],
): Promise<{ outcome: 'renamed' | 'unchanged' | 'unknown'; baseline: Route[] }> {
  const actual = new Map((await read()).map((route) => [route.alias, route]));
  const source = actual.get(readback.source.alias);
  const target = actual.get(readback.target_alias);
  if (source) return { outcome: 'unchanged', baseline };
  if (!route_matches(target, { ...readback.source, alias: readback.target_alias })) return { outcome: 'unknown', baseline };
  return {
    outcome: 'renamed',
    baseline: baseline.map((route) => route.alias === readback.source.alias ? target! : route),
  };
}

/** Reconcile only the submitted aliases. An unrelated concurrent route never
 * becomes a staged deletion merely because it appeared during this read. */
export async function read_route_writeback(
  readback: Route_write_readback,
  baseline: Route[],
  read: Route_storage['read'],
): Promise<Route_write_receipt> {
  const actual = new Map((await read()).map((route) => [route.alias, route]));
  const known = new Map(baseline.map((route) => [route.alias, route]));
  const failed_aliases: string[] = [];
  const persisted_after_rejection: string[] = [];
  const rejected = new Set(readback.rejected_aliases);
  const changes = [
    ...readback.plan.to_save.map((route) => ({ alias: route.alias, matches: route_matches(actual.get(route.alias), route) })),
    ...readback.plan.to_delete.map((route) => ({ alias: route.alias, matches: !actual.has(route.alias) })),
  ];
  for (const change of changes) {
    const stored = actual.get(change.alias);
    if (stored) known.set(change.alias, stored);
    else known.delete(change.alias);
    if (!change.matches) failed_aliases.push(change.alias);
    else if (rejected.has(change.alias)) persisted_after_rejection.push(change.alias);
  }
  return {
    baseline: [...known.values()],
    saved_count: changes.length - failed_aliases.length,
    failed_aliases,
    persisted_after_rejection,
    pending_readback: null,
  };
}

/** A rejected command can follow a committed DAO write and a failed runtime
 * apply. Settle every command and read its selected write scope before retry. */
export async function persist_route_changes(
  plan: Route_write_plan,
  baseline: Route[],
  storage: Route_storage,
): Promise<Route_write_receipt> {
  if (plan.to_save.length === 0 && plan.to_delete.length === 0) {
    return { baseline, saved_count: 0, failed_aliases: [], persisted_after_rejection: [], pending_readback: null };
  }
  const outcomes = await Promise.allSettled([
    ...plan.to_save.map((route) => storage.save(route)),
    ...plan.to_delete.map((route) => storage.remove(route.alias)),
  ]);
  const known = new Map(baseline.map((route) => [route.alias, route]));
  const rejected_aliases: string[] = [];
  outcomes.forEach((outcome, index) => {
    const saved_route = plan.to_save[index];
    const alias = saved_route?.alias ?? plan.to_delete[index - plan.to_save.length].alias;
    if (outcome.status === 'rejected') rejected_aliases.push(alias);
    else if (saved_route) known.set(alias, saved_route);
    else known.delete(alias);
  });
  const readback = { plan, rejected_aliases };
  try {
    return await read_route_writeback(readback, [...known.values()], storage.read);
  } catch {
    return {
      baseline: [...known.values()],
      saved_count: outcomes.length - rejected_aliases.length,
      failed_aliases: [],
      persisted_after_rejection: [],
      pending_readback: readback,
    };
  }
}
