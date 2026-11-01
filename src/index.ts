import { Command } from 'commander';
import chalk from 'chalk';
import { interactiveMenu } from './cli/menu.js';
import { startGatewayCmd, statusCmd, switchCmd, testCmd, modelsCmd, configPathCmd } from './cli/commands.js';

const program = new Command();

program
  .name('aag')
  .description(chalk.bold('AI Agent Gateway') + ' — unified proxy + switcher for Copilot/Kilo/Cursor/ZCode/MiniMax/Kimi')
  .version('0.1.0')
  .option('--menu', 'launch interactive TUI menu (default if no subcommand)')
  // Root action = the interactive menu default. Without a root action,
  // commander prints help and exits 1 during parse when no subcommand is
  // selected, so the no-args menu default would never run.
  .action(() => {
    interactiveMenu().catch((e) => {
      console.error(chalk.red(e.message));
      process.exit(1);
    });
  });

program
  .command('gateway', { isDefault: false })
  .description('start the gateway proxy server')
  // No -h short flag: it would shadow commander's built-in help.
  .option('--host <host>')
  .option('-p, --port <port>')
  .action(startGatewayCmd);

program
  .command('switch [agent]')
  .description('point an agent at the gateway or a direct provider')
  .option('-m, --model <model>')
  .option('--to <target>', 'gateway | direct | profile')
  .option('--base-url <url>')
  .option('--api-key <key>')
  .option('--profile <id>')
  .action(switchCmd);

program
  .command('status')
  .description('show current providers, routes, agents')
  .action(statusCmd);

program
  .command('models')
  .description('list available models (requires gateway running)')
  .action(modelsCmd);

program
  .command('test [model]')
  .description('send a mock test request to the gateway')
  .option('--no-stream')
  .action(testCmd);

program
  .command('config-path')
  .description('print the config file path')
  .action(configPathCmd);

program.parseAsync(process.argv).catch((e) => {
  console.error(chalk.red(e.message));
  process.exit(1);
});
