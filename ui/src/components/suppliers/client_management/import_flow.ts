import type { ImportReport } from '../../../lib/api/types.js';

export interface Run_cc_switch_import_options {
  running: boolean;
  import_providers: () => Promise<ImportReport>;
  stop_proxy: () => Promise<void>;
  start_proxy: () => Promise<string>;
  refresh_page: () => Promise<void>;
  translate: (key: string, vars?: Record<string, string | number>) => string;
  flash: (message: string) => void;
  set_importing: (value: boolean) => void;
  set_error: (value: string | null) => void;
}

export async function run_cc_switch_import(
  options: Run_cc_switch_import_options,
): Promise<void> {
  options.set_importing(true);
  options.set_error(null);
  try {
    const report = await options.import_providers();
    const no_key_count = report.skippedNoKey?.length ?? 0;
    if (
      report.imported.length === 0 &&
      report.skippedExisting.length === 0 &&
      no_key_count === 0
    ) {
      options.flash(options.translate('providers.importCcSwitch.empty'));
    } else {
      // importedRoutes may be empty for non-Claude imports; count distinct
      // aliases so multiple provider mappings to one route are reported once.
      const routes = report.importedRoutes ?? [];
      const route_count = new Set(routes.map((route) => route.alias)).size;
      const queued_count = report.queuedProviders?.length ?? 0;
      const passthrough_count = report.passthroughProviders?.length ?? 0;
      let restarted = false;
      if (options.running && report.imported.length > 0) {
        try {
          await options.stop_proxy();
        } catch {
          // A stop error does not prevent the restart attempt.
        }
        await options.start_proxy();
        restarted = true;
      }
      options.flash(
        options.translate('providers.importCcSwitch.report', {
          imported: String(report.imported.length),
          skipped: String(report.skippedExisting.length),
          total: String(report.totalInSource),
        }) +
          (route_count > 0
            ? ` · ${options.translate('providers.importCcSwitch.routes', { n: String(route_count) })}`
            : '') +
          (no_key_count > 0
            ? ` · ${options.translate('providers.importCcSwitch.noKey', { n: String(no_key_count) })}`
            : '') +
          (queued_count > 0
            ? ` · ${options.translate('providers.importCcSwitch.queued', { n: String(queued_count) })}`
            : '') +
          (passthrough_count > 0
            ? ` · ${options.translate('providers.importCcSwitch.passthrough', { n: String(passthrough_count) })}`
            : '') +
          (restarted ? ` · ${options.translate('providers.importCcSwitch.restarted')}` : ''),
      );
    }
    await options.refresh_page();
  } catch (error) {
    options.set_error(
      options.translate('providers.importCcSwitch.failed', { message: String(error) }),
    );
  } finally {
    options.set_importing(false);
  }
}
