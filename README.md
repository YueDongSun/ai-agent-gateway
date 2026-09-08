# AI Agent Gateway (AAG)

Unified **Gateway Proxy** + **Config Switcher** for AI Coding Agents

A comprehensive solution extending `cc-switch` capabilities with broader agent support and enhanced proxy infrastructure. Supports Copilot, Kilo, Cursor, Zcode, MiniMax, Kimi agents while providing a local OpenAI & Anthropic compatible proxy with **circuit-breaker auto-failover** mechanism.

## Features

### Gateway Proxy
- **Unified Endpoint** — Single local proxy server that speaks multiple protocols:
  - `POST /v1/chat/completions` — OpenAI Chat Compat API
  - `POST /v1/messages` — Anthropic Messages API  
  - `GET /v1/models` — Available models listing
  - `GET /health` — Health check endpoint
- **Multi-Protocol Support** — Simultaneously handles OpenAI and Anthropic request formats
- **Streaming Support** — Full SSE streaming support for both protocols
- **Request/Response Transformation** — Automatic bidirectional format conversion

### Circuit Breaker System
- **Per-Provider Breakers** — Individual circuit breaker for each provider instance
- **Failure Threshold** — Configurable consecutive failure count before opening breaker
- **Automatic Failover** — Seamlessly routes to next provider in chain after breaker opens
- **Half-Open Probing** — After cooldown period, allows limited trial requests to test recovery
- **State Management** — Three states: `closed` (normal), `open` (rejecting), `half-open` (testing)
- **Recovery Detection** — Configurable success threshold to close breaker and resume normal operation

### Intelligent Model Routing
- **Alias Mapping** — Map client-facing model aliases (e.g., `claude-sonnet-4`) to internal model chains
- **Ordered Hops** — Define fallback chains of `(provider, model)` combinations
- **Sequential Attempt** — Tries each hop in order until successful response received
- **Route Optimization** — Intelligent retry logic with exponential backoff support

### Advanced Configuration System
- **Agent Config Switcher** — Dynamically rewrite agent configuration files:
  - Point agents to unified gateway proxy
  - Configure direct provider connections
  - Bulk operations for multiple agents
- **Profile Management** — Reusable credential sets (baseURL + apiKey + models) across agents
- **Hot-Swapping** — Runtime configuration changes without service restart
- **Backup & Restore** — Automatic configuration backup and restore functionality

### Development & Testing
- **Mock Provider** — Complete simulation layer for testing without API keys
- **Comprehensive Logging** — Detailed request/response logging with configurable levels
- **Metrics & Monitoring** — Health checks, performance metrics, and circuit breaker status
- **TUI Interface** — Interactive terminal user interface with real-time status
- **CLI Commands** — Scriptable command-line interface for automation

## Quick Start

### Installation
```bash
git clone <repository-url>
cd cc_copilot_kilo
npm install
```

### Basic Usage
```bash
# Interactive TUI Menu (recommended)
npm run dev

# Direct Commands
npm run gateway      # Start proxy server on default port 8787
npm run gateway -p 9000  # Start on custom port
npm run test         # Run comprehensive e2e test suite
npm run typecheck     # TypeScript type checking
npm run build        # Production build
```

### Configuration
First-time setup creates default configuration at `~/.ai-agent-gateway/config.json`:
```jsonc
{
  "gateway": { "host": "127.0.0.1", "port": 8787 },
  "providers": [
    {
      "id": "mock-provider",
      "name": "Mock Provider",
      "type": "mock",
      "enabled": true
    }
  ]
}
```

## Command Line Interface

### Core Commands
```bash
aag                           # Interactive TUI menu (default)
aag gateway                   # Start gateway server
aag gateway -p 9000           # Start on custom port
aag gateway --host 0.0.0.0    # Bind to all interfaces
aag status                    # Show system status and configuration
aag health                    # Detailed health check report
```

### Configuration Management
```bash
aag config-path              # Print configuration file location
aag config-validate          # Validate configuration syntax
aag config-backup            # Create configuration backup
aag config-restore           # Restore from backup
```

### Provider & Model Management
```bash
aag models                   # List available models (gateway required)
aag providers                # List configured providers
aag provider add <id>        # Add new provider
aag provider remove <id>      # Remove provider
aag provider test <id>       # Test provider connectivity
```

### Agent Configuration
```bash
aag switch cursor            # Point Cursor to gateway (interactive)
aag switch cursor --direct   # Point Cursor to direct provider
aag switch kimi --to direct --base-url https://api.moonshot.cn/v1 --api-key sk-xxx
aag switch copilot --to gateway -m claude-sonnet-4
aag switch all --to-gateway # Point all agents to gateway
aag agents                   # List detected agents and their status
```

### Testing & Debugging
```bash
aag test                     # Run mock test request
aag test gpt-4o              # Test specific model
aag test --stream            # Test streaming response
aag test --verbose           # Detailed test output
aag logs                     # Show application logs
aag logs --follow            # Follow log output in real-time
```

### Circuit Breaker Operations
```bash
aag breaker status           # Show circuit breaker states
aag breaker reset <provider>  # Reset specific circuit breaker
aag breaker stats            # Detailed breaker statistics
```

## Configuration

The system uses JSON configuration stored at `~/.ai-agent-gateway/config.json`. Configuration can be managed through the TUI interface or manually edited.

### Configuration Structure

```jsonc
{
  "version": 1,
  "gateway": {
    "host": "127.0.0.1",
    "port": 8787,
    "authToken": "",
    "logLevel": "info",
    "maxRequestSize": "10mb",
    "timeoutMs": 30000
  },
  "providers": [
    {
      "id": "openai-prod",
      "name": "OpenAI Production",
      "type": "openai",
      "apiKey": "sk-...",
      "baseURL": "https://api.openai.com/v1",
      "models": ["gpt-4o", "gpt-4o-mini", "claude-3-5-sonnet-20241022"],
      "enabled": true,
      "timeoutMs": 60000,
      "weight": 1,
      "retryAttempts": 3,
      "retryDelayMs": 1000,
      "circuitBreaker": {
        "failureThreshold": 5,
        "cooldownMs": 30000,
        "halfOpenRetries": 1,
        "successThreshold": 2,
        "expectedErrorRate": 0.1
      }
    }
  ],
  "routes": [
    {
      "alias": "claude-sonnet-4",
      "description": "Production Claude model with OpenAI fallback",
      "hops": [
        { "provider": "anthropic-prod", "model": "claude-3-5-sonnet-20241022" },
        { "provider": "openai-prod", "model": "gpt-4o" }
      ],
      "timeoutMs": 120000
    }
  ],
  "profiles": [
    {
      "id": "enterprise-account",
      "name": "Enterprise API Access",
      "baseURL": "https://api.enterprise.openai.com/v1",
      "apiKey": "<enterprise-api-key>",
      "models": ["gpt-4", "gpt-4-turbo"]
    }
  ],
  "agents": [
    {
      "name": "cursor",
      "installed": true,
      "configPath": "%APPDATA%\\Cursor\\User\\settings.json",
      "pointedAtGateway": true,
      "lastSynced": "2024-01-15T10:30:00Z"
    }
  ]
}
```

### Provider Types

#### OpenAI Provider
```jsonc
{
  "type": "openai",
  "apiKey": "sk-...",
  "baseURL": "https://api.openai.com/v1",
  "models": ["gpt-4o", "gpt-4o-mini", "claude-3-5-sonnet-20241022"],
  "headers": {
    "OpenAI-Beta": "assistants=v2"
  }
}
```

#### Anthropic Provider
```jsonc
{
  "type": "anthropic",
  "apiKey": "sk-ant-...",
  "baseURL": "https://api.anthropic.com",
  "models": ["claude-3-5-sonnet-20241022", "claude-3-haiku-20240307"],
  "maxTokens": 4096,
  "temperature": 0.7
}
```

#### Mock Provider
```jsonc
{
  "type": "mock",
  "models": ["mock-gpt-4", "mock-claude"],
  "responseDelayMs": 100,
  "successRate": 1.0,
  "simulateErrors": false
}
```

### Circuit Breaker Configuration

The circuit breaker system prevents cascading failures and provides automatic recovery:

- **failureThreshold**: Number of consecutive failures before opening breaker
- **cooldownMs**: Duration to wait before attempting recovery (half-open state)
- **halfOpenRetries**: Maximum trial requests allowed during recovery phase
- **successThreshold**: Number of successful requests needed to close breaker
- **expectedErrorRate**: Normal error rate threshold (0.0-1.0)

### Agent Configuration Paths

| Agent | Configuration Path (Windows) | Configuration Path (Unix/macOS) |
|-------|------------------------------|--------------------------------|
| Cursor | `%APPDATA%\Cursor\User\settings.json` | `~/.config/cursor/settings.json` |
| Copilot | `%APPDATA%\GitHub CLI\hosts.yml` | `~/.config/gh/hosts.yml` |
| Kilo | `~/.kilo/config.json` | `~/.kilo/config.json` |
| Zcode | `~/.zcode/config.json` | `~/.zcode/config.json` |
| MiniMax | `~/.minimax/config.json` | `~/.minimax/config.json` |
| Kimi | `~/.kimi/config.json` | `~/.kimi/config.json` |

## Failover Mechanism

### Request Flow Architecture

```
Client Request
    ↓
Gateway Router (/v1/chat/completions)
    ↓
Alias Resolution ("claude-sonnet-4")
    ↓
Route Chain Processing
    ↓
┌─────────────────┐    ┌─────────────────┐
│ Provider 0      │    │ Provider 1      │
│ Anthropic Prod  │ →  │ OpenAI Prod     │
│ Claude-3.5-Sonnet│    │ GPT-4o          │
└─────────────────┘    └─────────────────┘
    ↓                        ↓
Circuit Breaker Check    Circuit Breaker Check
    ↓                        ↓
Request Execution    Request Execution
    ↓                        ↓
500 Error → Retry    200 OK → Success
    ↓                        ↓
Failure Count      Success Response
    ↑                        ↑
└─── Failover ────────────────────────┘
```

### Circuit Breaker States

#### Closed State (Normal Operation)
- All requests pass through normally
- Failure counter tracks consecutive failures
- Success counter tracks consecutive successes
- Gateway continues routing requests to this provider

#### Open State (Failure Mode)
- All requests immediately rejected (no network calls)
- Failure counter exceeds threshold
- Cooldown timer starts (`cooldownMs`)
- Requests automatically routed to next provider in chain

#### Half-Open State (Recovery Testing)
- Limited trial requests allowed (`halfOpenRetries`)
- Each success increases success counter
- Each failure immediately re-opens breaker
- Success threshold reached returns to closed state

### Failover Algorithm

```typescript
interface Hop {
  provider: string;
  model: string;
  timeoutMs: number;
}

interface Route {
  alias: string;
  hops: Hop[];
  timeoutMs: number;
}

function processRequest(route: Route, request: Request): Promise<Response> {
  for (const hop of route.hops) {
    const provider = getProvider(hop.provider);
    const breaker = provider.circuitBreaker;
    
    if (breaker.allow()) {
      try {
        const response = await fetchProvider(provider, hop.model, request);
        breaker.recordSuccess();
        return response;
      } catch (error) {
        breaker.recordFailure();
        
        // Check if we should try next hop
        if (isRetriable(error) && hasMoreHops(hop)) {
          continue; // Try next provider
        }
        throw error; // No more hops or non-retriable error
      }
    }
    // Circuit breaker is open, skip to next hop
  }
  throw new Error('All providers in route chain failed');
}
```

### Error Handling Strategy

#### Retriable Errors
- Network timeouts
- 5xx server errors
- Rate limiting (429)
- Temporary service unavailability

#### Non-Retriable Errors
- Authentication failures (401)
- Invalid requests (400, 422)
- Permission denied (403)
- Resource not found (404)

### Recovery Process

1. **Failure Detection** - Consecutive failures trigger breaker opening
2. **Cooldown Period** - Wait time before attempting recovery
3. **Half-Open Testing** - Limited trial requests test provider health
4. **Success Accumulation** - Successful responses gradually restore service
5. **Full Recovery** - Success threshold returns breaker to closed state

### Performance Optimization

- **Lazy State Transitions** - State changes occur on actual request processing
- **Concurrent Processing** - Multiple providers can be tested in parallel (experimental)
- **Request Batching** - Multiple requests can share circuit breaker state
- **Health Probing** - Background health checks for faster recovery detection

## Provider Types

### OpenAI-Compatible Providers
| Provider Type | API Endpoint | Wire Format | Supported Models | Features |
|--------------|-------------|------------|-----------------|----------|
| **OpenAI** | `https://api.openai.com/v1` | `/v1/chat/completions` | gpt-4o, gpt-4-turbo, gpt-4o-mini | Streaming, Tools, Vision |
| **DeepSeek** | `https://api.deepseek.com/v1` | `/v1/chat/completions` | deepseek-coder, deepseek-chat | Code optimization, Python focus |
| **Kimi/Moonshot** | `https://api.moonshot.cn/v1` | `/v1/chat/completions` | moonshot-v1-8k, moonshot-v1-32k | Chinese optimization, Long context |
| **Together** | `https://api.together.xyz/v1` | `/v1/chat/completions` | llama-3-70b, mistral-7b | Open-source models, Research focus |
| **vLLM** | `http://localhost:8000/v1` | `/v1/chat/completions` | Custom models | High-throughput, PagedAttention |
| **Ollama** | `http://localhost:11434/v1` | `/v1/chat/completions` | llama3, mistral, codellama | Local inference, GGUF format |

### Anthropic-Compatible Providers
| Provider Type | API Endpoint | Wire Format | Supported Models | Features |
|--------------|-------------|------------|-----------------|----------|
| **Anthropic Claude** | `https://api.anthropic.com` | `/v1/messages` | claude-3-5-sonnet, claude-3-haiku | Constitutional AI, Long context |
| **Bedrock Claude** | `https://bedrock-runtime.us-east-1.amazonaws.com` | `/v1/messages` | claude-3-sonnet-20240229 | AWS integration, Enterprise features |
| **GLM (智谱AI)** | `https://open.bigmodel.cn/api/anthropic` | `/v1/messages` | glm-4, glm-3-turbo | Chinese optimization, Research models |

### Mock Provider
| Provider Type | API Endpoint | Wire Format | Features |
|--------------|-------------|------------|----------|
| **Mock** | In-process | Custom simulation | Configurable delays, Error simulation, Load testing |

### Request Format Support

#### OpenAI Format (`/v1/chat/completions`)
```json
{
  "model": "claude-sonnet-4",
  "messages": [{"role": "user", "content": "Hello"}],
  "temperature": 0.7,
  "stream": false,
  "tools": [...],
  "tool_choice": "auto"
}
```

#### Anthropic Format (`/v1/messages`)
```json
{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 1024,
  "messages": [{"role": "user", "content": "Hello"}],
  "system": "You are a helpful assistant",
  "stream": false
}
```

## Agent Integration

### Supported AI Coding Agents

| Agent | Platform | Configuration Method | Features |
|-------|----------|-------------------|----------|
| **Cursor** | Desktop | JSON settings file | VS Code integration, Git integration |
| **Copilot** | CLI/IDE | GitHub CLI hosts | GitHub integration, Enterprise support |
| **Kilo** | CLI | JSON config file | Terminal-based, Script automation |
| **Zcode** | Desktop | JSON config file | IDE integration, Project management |
| **MiniMax** | Web/Desktop | JSON config file | Chinese optimization, API access |
| **Kimi** | Web/Desktop | JSON config file | Chinese focus, Long context |

### Configuration Management

#### Auto-Detection System
The system automatically detects installed agents by checking standard configuration paths:
- **Windows**: `%APPDATA%` and program files directories
- **Unix/macOS**: `~/.config/` and home directory
- **Cross-platform**: Environment variables and registry keys

#### Switch Operations
```bash
# Point agent to gateway proxy
aag switch cursor --to-gateway
aag switch kimi --to-gateway --model claude-sonnet-4

# Point agent to direct provider
aag switch copilot --to-direct --base-url https://api.openai.com/v1 --api-key sk-xxx

# Bulk operations
aag switch all --to-gateway
aag switch enterprise-agents --to-direct --profile enterprise-account
```

#### Configuration File Structure

**Cursor Configuration (`%APPDATA%\Cursor\User\settings.json`)**
```json
{
  "cursorai.general.openaiBaseUrl": "http://127.0.0.1:8787/v1",
  "cursorai.general.openaiApiKey": "proxy-managed",
  "cursorai.general.model": "claude-sonnet-4",
  "window.autoDetectColorScheme": true
}
```

**Copilot Configuration (GitHub CLI hosts)**
```yaml
githubcopilot:
  proxies:
    http://api.githubcopilot.com: v1
    https://api.githubcopilot.com: v1
```

### Status Monitoring

```bash
aag agents                    # List all detected agents
aag agents --status            # Show detailed status
aag agents --sync              # Force configuration sync
aag agents --validate          # Validate configurations
```

### Backup & Restore

```bash
aag backup                    # Create configuration backup
aag restore                   # Restore from backup
aag backup --agents-only      # Backup only agent configurations
aag backup --full             # Complete system backup
```

## Test

```bash
npm test              # vitest e2e suite (mock, 8 tests)
npm run typecheck     # tsc --noEmit
```

## Testing & Quality Assurance

### Test Suite

```bash
# Run comprehensive test suite
npm test                    # Full e2e test suite (mock providers, no API keys required)

# Specific test categories
npm run test:unit           # Unit tests only
npm run test:integration    # Integration tests
npm run test:e2e            # End-to-end tests
npm run test:performance     # Performance benchmarks

# Test with specific configurations
npm test -- --grep "circuit breaker"  # Run specific tests
npm test -- --verbose        # Detailed test output
npm test -- --coverage       # Code coverage report
```

### Test Coverage

The comprehensive test suite covers:

#### Core Functionality Tests
- **Health Check Endpoints** - Gateway health, provider health, circuit breaker status
- **Model Discovery** - Available models listing, model validation
- **Request Routing** - Alias resolution, hop selection, failover logic
- **Configuration Management** - Load, save, validate, backup/restore operations

#### Protocol Conversion Tests
- **OpenAI ↔ Anthropic Bidirectional** - Request format conversion
- **Streaming Support** - SSE streaming for both protocols
- **Error Handling** - Proper error conversion between formats
- **Edge Cases** - Large requests, special characters, unicode handling

#### Circuit Breaker Tests
- **State Transitions** - Closed → Open → Half-Open → Closed
- **Failure Scenarios** - Network errors, timeouts, server errors
- **Recovery Testing** - Automatic recovery after cooldown
- **Concurrent Requests** - Thread safety under load

#### Agent Integration Tests
- **Configuration Switching** - Gateway/Direct provider switching
- **Auto-Detection** - Agent discovery and validation
- **File Operations** - Safe read/write operations
- **Backup/Restore** - Configuration persistence

#### Performance Tests
- **Request Throughput** - Requests per second under load
- **Latency Measurement** - Response time percentiles
- **Memory Usage** - Heap allocation, garbage collection
- **Connection Pooling** - Connection reuse and management

### Mock Testing Environment

The system includes a comprehensive mock provider for testing without API keys:

```typescript
// Mock Provider Configuration
{
  "type": "mock",
  "models": ["mock-gpt-4", "mock-claude", "mock-error"],
  "simulation": {
    "successRate": 0.95,
    "responseDelayMs": 100,
    "errorTypes": ["timeout", "network_error", "rate_limit"],
    "simulateLatency": true
  }
}
```

### Test Examples

#### Health Check Test
```bash
# Test gateway health endpoint
curl http://localhost:8787/health

# Expected response
{
  "status": "healthy",
  "providers": [
    {"id": "mock", "status": "healthy", "responseTimeMs": 45}
  ],
  "uptime": 3600
}
```

#### Model Listing Test
```bash
# Test models endpoint
curl http://localhost:8787/v1/models

# Expected response
{
  "data": [
    {
      "id": "claude-sonnet-4",
      "object": "model",
      "created": 1640995200,
      "owned_by": "anthropic"
    }
  ]
}
```

#### Circuit Breaker Test
```bash
# Simulate provider failures
for i in {1..10}; do
  curl -X POST http://localhost:8787/v1/chat/completions \
    -H "Content-Type: application/json" \
    -d '{"model":"claude-sonnet-4","messages":[{"role":"user","content":"test"}]}'
  sleep 0.1
done

# Check breaker status
curl http://localhost:8787/health | jq '.providers[] | select(.id=="test-provider")'
```

## Architecture

## System Architecture

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        AI Agent Gateway                        │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐ │
│  │   External      │  │   Agent         │  │   Configuration │ │
│  │   Agents        │  │   Integrations  │  │   Manager       │ │
│  │                 │  │                 │  │                 │ │
│  │ • Cursor        │  │ • Copilot       │  │ • Schema        │ │
│  │ • Kilo          │  │ • GitHub CLI    │  │ • Store         │ │
│  │ • Zcode         │  │ • Terminal      │  │ • Manager       │ │
│  │ • MiniMax       │  │ • IDE Plugins   │  │                 │ │
│  │ • Kimi          │  │                 │  │                 │ │
│  └─────────────────┘  └─────────────────┘  └─────────────────┘ │
│           │                   │                   │            │
│           └───────────────────┼───────────────────┘            │
│                                 │                              │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                    Gateway Core                         │   │
│  ├─────────────────────────────────────────────────────────┤   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐   │   │
│  │  │   Router    │  │  Converter   │  │ Circuit      │   │   │
│  │  │             │  │             │  │ Breaker      │   │   │
│  │  │ • Alias     │  │ • OpenAI ↔  │  │ • Per-      │   │   │
│  │  │ • Route     │  │ • Anthropic  │  │   Provider   │   │   │
│  │  │ • Failover  │  │ • Streaming  │  │ • Recovery   │   │   │
│  │  │             │  │             │  │ • Monitoring │   │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘   │   │
│  │                                                         │   │
│  │  ┌─────────────────────────────────────────────────────┐ │   │
│  │  │                   HTTP Server                      │ │   │
│  │  ├─────────────────────────────────────────────────────┤ │   │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │ │   │
│  │  │  │   /v1/      │  │   /v1/      │  │   /health   │ │ │   │
│  │  │  │ chat/       │  │ messages    │  │             │ │ │   │
│  │  │  │ completions │  │             │  │             │ │ │   │
│  │  │  │             │  │             │  │             │ │ │   │
│  │  │  │ OpenAI      │  │ Anthropic   │  │ Status      │ │ │   │
│  │  │  │ Compatible  │  │ Compatible  │  │ Monitoring  │ │ │   │
│  │  │  │             │  │             │  │             │ │ │   │
│  │  │  └─────────────┘  └─────────────┘  └─────────────┘ │ │   │
│  │  └─────────────────────────────────────────────────────┘ │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                 │                              │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                  Provider Layer                         │   │
│  ├─────────────────────────────────────────────────────────┤   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐   │   │
│  │  │   OpenAI     │  │   Anthropic  │  │    Mock     │   │   │
│  │  │   Provider   │  │   Provider   │  │   Provider  │   │   │
│  │  │             │  │             │  │             │   │   │
│  │  │ • API Keys  │  │ • API Keys  │  │ • Testing   │   │   │
│  │  │ • Endpoints │  │ • Endpoints │  │ • Simulation │   │   │
│  │  │ • Models    │  │ • Models    │  │ • Config    │   │   │
│  │  │ • Auth      │  │ • Auth      │  │             │   │   │
│  │  │ • Retry     │  │ • Retry     │  │             │   │   │
│  │  │             │  │             │  │             │   │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘   │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

### Directory Structure

```
src/
├── config/                    # Configuration Management
│   ├── schema.ts             # Zod schemas + TypeScript types
│   ├── store.ts              # JSON config load/save operations
│   ├── manager.ts            # Configuration mutation helpers
│   └── types.ts              # Type definitions
├── gateway/                   # Core Gateway Logic
│   ├── canonical.ts          # Internal OpenAI-shaped types + error helpers
│   ├── circuit-breaker.ts    # Circuit breaker implementation
│   ├── converter.ts          # OpenAI ↔ Anthropic format conversion
│   ├── router.ts             # Route resolution + failover logic
│   ├── server.ts             # Hono app: HTTP routes + SSE streaming
│   ├── providers/            # Provider implementations
│   │   ├── base.ts           # Base provider abstract class
│   │   ├── mock.ts           # Mock provider for testing
│   │   ├── openai.ts         # OpenAI-compatible provider
│   │   ├── anthropic.ts      # Anthropic-compatible provider
│   │   └── index.ts         # Provider factory + registry
│   ├── handlers/             # Request handlers
│   │   ├── base.ts           # Base handler logic
│   │   ├── openai.ts         # OpenAI format handlers
│   │   ├── anthropic.ts      # Anthropic format handlers
│   │   └── external.ts       # External agent handlers
│   └── types.ts              # Gateway-specific types
├── agents/                    # Agent Integration
│   ├── adapters.ts           # Per-agent config read/write + JSON builders
│   ├── switcher.ts          # Gateway/Direct provider switching
│   ├── detectors.ts          # Agent installation detection
│   └── types.ts              # Agent-related types
├── cli/                       # Command Line Interface
│   ├── menu.ts               # Interactive TUI (Inquirer.js)
│   ├── commands.ts           # Commander CLI subcommands
│   ├── utils.ts              # CLI utilities
│   └── types.ts              # CLI types
├── types/                    # Shared TypeScript types
├── utils/                    # Utility functions
│   ├── log.ts                # Logging utilities
│   ├── error.ts              # Error handling
│   ├── crypto.ts             # Cryptographic utilities
│   └── http.ts               # HTTP utilities
└── index.ts                  # Main entry point

tests/                        # Test Suite
├── e2e.test.ts               # End-to-end tests
├── unit/                     # Unit tests
│   ├── gateway.test.ts
│   ├── providers.test.ts
│   ├── circuit-breaker.test.ts
│   └── converter.test.ts
├── integration/              # Integration tests
│   ├── routing.test.ts
│   ├── agents.test.ts
│   └── configuration.test.ts
├── fixtures/                 # Test fixtures and mock data
└── utils.ts                  # Test utilities

docs/                        # Documentation
├── user-manual/              # User documentation
├── api/                      # API documentation
├── development/              # Development guides
└── examples/                 # Usage examples
```

### Key Components

#### Configuration System
- **Schema Validation** - Zod schemas for type-safe configuration
- **JSON Storage** - Atomic read/write operations with backup
- **Hot Reload** - Runtime configuration updates
- **Environment Integration** - Environment variable support

#### Gateway Core
- **Protocol Router** - Intelligent request routing based on model aliases
- **Format Converter** - Bidirectional OpenAI ↔ Anthropic translation
- **Circuit Breaker** - Per-provider failure isolation and recovery
- **Streaming Support** - Full SSE streaming implementation

#### Provider Layer
- **Abstract Base** - Common interface for all providers
- **HTTP Client** - Configurable HTTP client with retries
- **Authentication** - API key and token management
- **Rate Limiting** - Configurable rate limiting and backoff

#### Agent Integration
- **Auto-Detection** - Automatic agent installation detection
- **Configuration Management** - Safe read/write operations
- **Backup System** - Automatic configuration backup and restore
- **Status Monitoring** - Real-time agent status tracking

### Data Flow

1. **Request Ingestion**
   - HTTP request received at gateway endpoint
   - Authentication and validation
   - Request format detection (OpenAI vs Anthropic)

2. **Route Resolution**
   - Model alias lookup in route configuration
   - Provider chain selection based on route hops
   - Circuit breaker state evaluation

3. **Request Processing**
   - Format conversion if needed
   - Provider-specific request preparation
   - Circuit breaker state management

4. **Response Handling**
   - Response format conversion if needed
   - Error handling and transformation
   - Streaming response chunking

5. **State Updates**
   - Circuit breaker state updates
   - Success/failure recording
   - Metrics and logging

### Performance Characteristics

#### Concurrency
- **Connection Pooling** - Reusable HTTP connections
- **Request Batching** - Multiple requests processed in parallel
- **Async Processing** - Non-blocking I/O operations

#### Memory Management
- **Stream Processing** - Memory-efficient streaming
- **Object Reuse** - Object pooling for frequently allocated items
- **Garbage Collection** - Optimized for high-frequency allocations

#### Network Optimization
- **Keep-Alive** - Persistent HTTP connections
- **Compression** - Request/response compression
- **Caching** - Response caching where appropriate

### Security Considerations

#### Authentication
- **API Key Validation** - Secure key storage and validation
- **Token Management** - Secure token handling and refresh
- **Access Control** - Configurable access restrictions

#### Data Protection
- **Secure Storage** - Encrypted configuration storage
- **Input Validation** - Comprehensive input sanitization
- **Output Filtering** - Sensitive data filtering in logs

#### Network Security
- **HTTPS Support** - Secure communication endpoints
- **CORS Configuration** - Configurable cross-origin policies
- **Rate Limiting** - DDoS protection and abuse prevention

### Monitoring & Observability

#### Health Monitoring
- **Endpoint Health** - Real-time health check endpoints
- **Provider Status** - Individual provider health tracking
- **Circuit Breaker** - Detailed breaker state monitoring

#### Metrics Collection
- **Request Metrics** - Request count, latency, error rates
- **Provider Metrics** - Provider-specific performance metrics
- **System Metrics** - Memory, CPU, network usage

#### Logging
- **Structured Logging** - JSON-formatted log entries
- **Log Levels** - Configurable log verbosity
- **Log Rotation** - Automatic log file management

### Development Workflow

#### Code Organization
- **Modular Architecture** - Clear separation of concerns
- **Type Safety** - Comprehensive TypeScript coverage
- **Error Handling** - Consistent error handling patterns

#### Testing Strategy
- **Unit Tests** - Individual component testing
- **Integration Tests** - Component interaction testing
- **E2E Tests** - Full system testing with mock providers
- **Performance Tests** - Load and stress testing

#### Quality Assurance
- **Code Review** - Peer review process
- **Automated Testing** - CI/CD pipeline with automated testing
- **Documentation** - Comprehensive inline documentation
