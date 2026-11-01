import { describe, expect, it } from 'vitest';
import { persist_route_changes, read_route_rename, read_route_writeback } from '../ui/src/components/suppliers/client_management/route_persistence.js';
import type { Route } from '../ui/src/lib/api/proxy.js';

const route = (alias: string, model = 'model-exact'): Route => ({ alias, description: '', hops: [{ provider: 'fixture', model }] });

describe('route persistence receipts', () => {
  it('waits for every write and keeps only the failed draft pending after partial persistence', async () => {
    const stored = new Map<string, Route>();
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    let completed = false;
    const pending = persist_route_changes({ to_save: [route('a'), route('b')], to_delete: [] }, [], {
      save: async (value) => { if (value.alias === 'a') throw new Error('fixture failure'); await gate; stored.set(value.alias, value); },
      remove: async (alias) => { stored.delete(alias); },
      read: async () => [...stored.values()],
    }).then((value) => { completed = true; return value; });
    await Promise.resolve(); await Promise.resolve();
    expect(completed).toBe(false);
    release();
    const receipt = await pending;
    expect(stored.has('b')).toBe(true);
    expect(receipt.baseline).toEqual([route('b')]);
    expect(receipt.failed_aliases).toEqual(['a']);
    expect(receipt.saved_count).toBe(1);
    expect(receipt.pending_readback).toBeNull();
  });

  it('recognizes a committed write even when later runtime application rejects', async () => {
    const stored = new Map<string, Route>();
    const receipt = await persist_route_changes({ to_save: [route('committed'), route('failed')], to_delete: [] }, [], {
      save: async (value) => { if (value.alias === 'committed') stored.set(value.alias, value); throw new Error('fixture apply failed'); },
      remove: async (alias) => { stored.delete(alias); },
      read: async () => [...stored.values()],
    });
    expect(receipt.baseline).toEqual([route('committed')]);
    expect(receipt.persisted_after_rejection).toEqual(['committed']);
    expect(receipt.failed_aliases).toEqual(['failed']);
  });

  it('retains unknown outcomes until readback and does not acquire unrelated rows for deletion', async () => {
    const stored = new Map<string, Route>();
    const plan = { to_save: [route('submitted')], to_delete: [route('removed')] };
    const first = await persist_route_changes(plan, [route('removed')], {
      save: async (value) => { stored.set(value.alias, value); throw new Error('fixture apply failed'); },
      remove: async (alias) => { stored.delete(alias); },
      read: async () => { throw new Error('fixture readback failed'); },
    });
    expect(first.pending_readback).not.toBeNull();
    stored.set('concurrent', route('concurrent'));
    const reconciled = await read_route_writeback(first.pending_readback!, first.baseline, async () => [...stored.values()]);
    expect(reconciled.baseline).toEqual([route('submitted')]);
    expect(reconciled.failed_aliases).toEqual([]);
    expect(reconciled.saved_count).toBe(2);
    expect(reconciled.persisted_after_rejection).toEqual(['submitted']);
    expect(stored.has('concurrent')).toBe(true);
  });

  it('compares the exact saved hop order, model, provider and description', async () => {
    const wanted = { ...route('changed'), description: 'new', hops: [{ provider: 'first', model: 'model-1' }, { provider: 'second', model: 'model-2' }] };
    const receipt = await read_route_writeback({ plan: { to_save: [wanted], to_delete: [] }, rejected_aliases: [] }, [], async () => [{ ...wanted, hops: [...wanted.hops].reverse() }]);
    expect(receipt.failed_aliases).toEqual(['changed']);
    expect(receipt.saved_count).toBe(0);
  });

  it('reconciles an atomic rename without borrowing an unrelated target alias', async () => {
    const source = route('old');
    const readback = { source, target_alias: 'new' };
    expect((await read_route_rename(readback, [source], async () => [route('new')])).outcome).toBe('renamed');
    expect((await read_route_rename(readback, [source], async () => [source, route('new')])).outcome).toBe('unchanged');
    expect((await read_route_rename(readback, [source], async () => [route('new', 'different-model')])).outcome).toBe('unknown');
    expect((await read_route_rename(readback, [source], async () => [])).outcome).toBe('unknown');
  });
});
