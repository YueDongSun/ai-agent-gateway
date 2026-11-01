import { z } from 'zod';

/**
 * The request contract is intentionally kept at the HTTP edge.  Provider
 * adapters may accept additional wire fields, but admission validates the
 * fields whose meaning is shared by every route.
 */
export const Chat_message_schema = z.object({
  role: z.string().min(1, 'role is required'),
  content: z.unknown(),
}).passthrough();

export const Chat_request_schema = z.object({
  model: z.string().trim().min(1, 'model is required'),
  messages: z.array(Chat_message_schema).min(1, 'at least one message is required'),
  stream: z.boolean().optional(),
  temperature: z.number().finite().min(0, 'temperature must be at least 0').max(2, 'temperature must be at most 2').optional(),
  max_tokens: z.number().int().min(1, 'max_tokens must be positive').max(8192, 'max_tokens is too large').optional(),
}).passthrough();

export type Chat_request = z.infer<typeof Chat_request_schema>;

export interface Field_validation_error {
  field: string;
  code: string;
  message: string;
}

export function validate_chat_request(value: unknown):
  | { ok: true; request: Chat_request }
  | { ok: false; errors: Field_validation_error[] } {
  const parsed = Chat_request_schema.safeParse(value);
  if (parsed.success) return { ok: true, request: parsed.data };
  return {
    ok: false,
    errors: parsed.error.issues.map((issue) => ({
      field: issue.path.length > 0 ? issue.path.map(String).join('.') : 'request',
      code: issue.code === 'too_small' && issue.path.at(-1) === 'model' ? 'required' : issue.code,
      message: issue.message,
    })),
  };
}
