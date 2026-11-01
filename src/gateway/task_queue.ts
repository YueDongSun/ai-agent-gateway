import { createHash, randomUUID } from 'node:crypto';

export interface Queue_task_input {
  user_id: string;
  model: string;
  prompt: string;
  idempotency_key?: string;
}

export interface Queue_task {
  task_id: string;
  user_id: string;
  model: string;
  prompt_hash: string;
  idempotency_key: string;
  status: 'queued' | 'generating' | 'completed' | 'failed';
  created_at: number;
}

export interface Queue_submission {
  task: Queue_task;
  duplicate: boolean;
  deduplication_notice?: string;
  idempotency_key_generated: boolean;
}

export interface Task_queue_options {
  validity_window_ms?: number;
  now?: () => number;
  id_factory?: () => string;
}

/**
 * Small storage-independent queue core.  A durable adapter can persist the
 * same keys under a unique (user_id, idempotency_key) constraint; keeping the
 * admission logic here makes concurrency and retry behavior testable without
 * requiring SQLite in unit tests.
 */
export class Task_queue {
  private readonly validity_window_ms: number;
  private readonly now: () => number;
  private readonly id_factory: () => string;
  private readonly tasks_by_idempotency = new Map<string, Queue_task>();
  private readonly tasks_by_content = new Map<string, Queue_task>();

  constructor(options: Task_queue_options = {}) {
    this.validity_window_ms = options.validity_window_ms ?? 24 * 60 * 60 * 1000;
    this.now = options.now ?? (() => Date.now());
    this.id_factory = options.id_factory ?? randomUUID;
  }

  static prompt_hash(prompt: string): string {
    return createHash('sha256').update(prompt, 'utf8').digest('hex');
  }

  submit(input: Queue_task_input): Queue_submission {
    const now = this.now();
    const prompt_hash = Task_queue.prompt_hash(input.prompt);
    const supplied_key = input.idempotency_key?.trim() ?? '';
    const idempotency_key = supplied_key || `${input.user_id}:${this.id_factory()}`;
    const idempotency_index = `${input.user_id}\u0000${idempotency_key}`;
    const content_index = `${input.user_id}\u0000${input.model}\u0000${prompt_hash}`;
    this.prune(now);

    const existing_by_key = this.tasks_by_idempotency.get(idempotency_index);
    if (existing_by_key) {
      return {
        task: existing_by_key,
        duplicate: true,
        deduplication_notice: 'Task already exists — taking you to the original task.',
        idempotency_key_generated: !supplied_key,
      };
    }

    // The content key prevents retries/reconnects from creating a second task
    // for the same user/model/prompt even when a client omitted a key.
    const existing_by_content = this.tasks_by_content.get(content_index);
    if (existing_by_content) {
      return {
        task: existing_by_content,
        duplicate: true,
        deduplication_notice: supplied_key
          ? 'Task already exists — taking you to the original task.'
          : 'A matching task already exists. Supply an Idempotency-Key for retry safety.',
        idempotency_key_generated: !supplied_key,
      };
    }

    const task: Queue_task = {
      task_id: this.id_factory(),
      user_id: input.user_id,
      model: input.model,
      prompt_hash,
      idempotency_key,
      status: 'queued',
      created_at: now,
    };
    this.tasks_by_idempotency.set(idempotency_index, task);
    this.tasks_by_content.set(content_index, task);
    return { task, duplicate: false, idempotency_key_generated: !supplied_key };
  }

  size(): number {
    this.prune(this.now());
    return this.tasks_by_content.size;
  }

  private prune(now: number): void {
    for (const task of this.tasks_by_content.values()) {
      if (now - task.created_at < this.validity_window_ms) continue;
      this.tasks_by_content.delete(`${task.user_id}\u0000${task.model}\u0000${task.prompt_hash}`);
      this.tasks_by_idempotency.delete(`${task.user_id}\u0000${task.idempotency_key}`);
    }
  }
}
