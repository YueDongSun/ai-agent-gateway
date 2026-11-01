import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { fileURLToPath } from 'node:url';
import { z } from 'zod';
import { default_descriptor_file, parse_server_args, workflow_config_limits, cursor_schema, Workflow_error } from './config.mjs';
import { workflow_owner_request } from './ipc.mjs';

const list_input_schema = z.object({
  cursor: cursor_schema.optional(),
  limit: z.number().int().min(1).max(16).optional(),
}).strict();
const health_input_schema = z.object({
  cursor: cursor_schema.optional(),
  limit: z.number().int().min(1).max(16).optional(),
}).strict();
const route_input_schema = z.object({ task: z.string().min(1).refine((task) => task.trim().length > 0) }).strict();

function error_result(error) {
  const code = error instanceof Workflow_error && /^workflow:[a-z_]+$/.test(error.code)
    ? error.code
    : 'workflow:owner_unavailable';
  return { isError: true, content: [{ type: 'text', text: code }] };
}

function result_value(result) {
  return { structuredContent: result, content: [{ type: 'text', text: JSON.stringify(result) }] };
}

function register_tool(server, name, description, schema, method, options) {
  server.registerTool(name, { description, inputSchema: schema, annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false } }, async (args) => {
    try {
      if (method === 'route_preview' && Buffer.byteLength(args.task, 'utf8') > 8192) {
        return error_result(new Workflow_error('workflow:invalid_params'));
      }
      const params = method === 'route_preview'
        ? { task: args.task }
        : method === 'list_capabilities' || method === 'get_health'
          ? { cursor: args.cursor, limit: args.limit }
          : {};
      const result = await (options.owner_request ?? workflow_owner_request)(method, params, options);
      return result_value(result);
    } catch (error) {
      return error_result(error);
    }
  });
}

export function create_workflow_server(options = {}) {
  const server = new McpServer({ name: 'aag_workflow_read_facade', version: '1.0.0' });
  register_tool(server, 'workflow_list_capabilities', 'Read one bounded capability catalog page (8 entries by default, limit 1-16). Continue with next_cursor to read subsequent pages.', list_input_schema, 'list_capabilities', options);
  register_tool(server, 'workflow_route_preview', 'Preview the deterministic route for a task description. This does not dispatch work.', route_input_schema, 'route_preview', options);
  register_tool(server, 'workflow_get_health', 'Read one bounded health observation page (8 entries by default, limit 1-16). Continue with next_cursor to read subsequent pages.', health_input_schema, 'get_health', options);
  return server;
}

export async function serve_workflow_mcp(options = {}) {
  const server = create_workflow_server(options);
  const transport = options.transport ?? new StdioServerTransport(process.stdin, process.stdout, {
    maxBufferSize: workflow_config_limits.max_frame_bytes,
  });
  await server.connect(transport);
  return server;
}

async function main(argv = process.argv.slice(2)) {
  const args = parse_server_args(argv);
  if (args.help) {
    process.stdout.write('Usage: node scripts/workflow/server.mjs [--descriptor-file <trusted-local-file>]\n');
    return;
  }
  await serve_workflow_mcp({ descriptor_file: args.descriptor_file ?? default_descriptor_file() });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch(() => { process.stderr.write('workflow:startup_failed\n'); process.exitCode = 1; });
}
