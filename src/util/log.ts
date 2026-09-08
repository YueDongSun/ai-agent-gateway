import process from 'node:process';

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

const ORDER: Record<LogLevel, number> = { debug: 10, info: 20, warn: 30, error: 40 };

let currentLevel: LogLevel = 'info';

export function setLogLevel(level: LogLevel): void {
  currentLevel = level;
}

function ts(): string {
  return new Date().toISOString().replace('T', ' ').replace('Z', '');
}

function log(level: LogLevel, tag: string, msg: string, extra?: unknown): void {
  if (ORDER[level] < ORDER[currentLevel]) return;
  const line = `${ts()} [${level.toUpperCase()}] ${tag}: ${msg}`;
  const stream = level === 'error' || level === 'warn' ? process.stderr : process.stdout;
  stream.write(line + '\n');
  if (extra !== undefined) {
    stream.write(JSON.stringify(extra) + '\n');
  }
}

export const logger = {
  setLevel: setLogLevel,
  debug: (tag: string, msg: string, extra?: unknown) => log('debug', tag, msg, extra),
  info: (tag: string, msg: string, extra?: unknown) => log('info', tag, msg, extra),
  warn: (tag: string, msg: string, extra?: unknown) => log('warn', tag, msg, extra),
  error: (tag: string, msg: string, extra?: unknown) => log('error', tag, msg, extra),
};
