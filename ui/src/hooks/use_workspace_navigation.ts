import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from 'react';
import { parse_workspace_route, type Workspace_view } from '../lib/workspace_routes.js';
import { request_workspace_navigation } from './use_navigation_block.js';

function browser_history_index(): number | undefined {
  const navigation = (window as Window & { navigation?: { currentEntry?: { index: number } } }).navigation;
  return navigation?.currentEntry?.index;
}

export function use_workspace_navigation(main_ref: RefObject<HTMLElement>) {
  const [view, set_view] = useState<Workspace_view>(() => parse_workspace_route(location.hash) ?? 'suppliers');
  const history_index = useRef<number>(browser_history_index() ?? (Number.isInteger(history.state?.aag_workspace_index) ? history.state.aag_workspace_index : 0));
  const history_length = useRef(history.length);
  const restoring = useRef(false);
  const navigate = useCallback((next: Workspace_view) => {
    if (next === view) return;
    if (!request_workspace_navigation()) return;
    history_index.current++;
    history.pushState({ aag_workspace_index: history_index.current }, '', `#/${next}`);
    history_index.current = browser_history_index() ?? history_index.current;
    history_length.current = history.length;
    set_view(next);
  }, [view]);

  useEffect(() => {
    if (!Number.isInteger(history.state?.aag_workspace_index)) {
      history.replaceState({ ...(history.state && typeof history.state === 'object' ? history.state : {}), aag_workspace_index: history_index.current }, '');
    }
    const restore = (event: PopStateEvent | HashChangeEvent) => {
      const next = parse_workspace_route(location.hash) ?? 'suppliers';
      let next_index = browser_history_index() ?? history.state?.aag_workspace_index;
      if (!Number.isInteger(next_index)) {
        // A directly assigned fragment has no app state. Label its entry before
        // undoing it so a later Forward has a stable direction as well.
        next_index = history_index.current + (history.length > history_length.current || event.type === 'hashchange' ? 1 : -1);
        history.replaceState({ ...(history.state && typeof history.state === 'object' ? history.state : {}), aag_workspace_index: next_index }, '');
      }
      history_length.current = history.length;
      if (restoring.current) {
        if (next_index === history_index.current) restoring.current = false;
        return;
      }
      if (next === view) return;
      if (!request_workspace_navigation()) {
        restoring.current = true;
        // Undo the traversal itself, retaining the destination entry for later.
        const delta = Number.isInteger(next_index) ? history_index.current - next_index : event.type === 'hashchange' ? -1 : 1;
        history.go(delta || 1);
        return;
      }
      history_index.current = Number.isInteger(next_index) ? next_index : history_index.current + 1;
      if (!Number.isInteger(next_index)) history.replaceState({ aag_workspace_index: history_index.current }, '');
      set_view(next);
    };
    const previous_restoration = history.scrollRestoration;
    history.scrollRestoration = 'manual';
    window.addEventListener('popstate', restore);
    window.addEventListener('hashchange', restore);
    return () => {
      window.removeEventListener('popstate', restore);
      window.removeEventListener('hashchange', restore);
      history.scrollRestoration = previous_restoration;
    };
  }, [view]);

  useLayoutEffect(() => {
    // Every destination opens at its heading, including Back/Forward. Do not
    // carry a previous provider's scroll position or focus into another app.
    window.scrollTo({ top: 0, behavior: 'instant' });
    main_ref.current?.focus({ preventScroll: true });
  }, [view, main_ref]);
  return { view, navigate };
}
