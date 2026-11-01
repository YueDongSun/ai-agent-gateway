import { useEffect, useRef } from 'react';

const navigation_event = 'aag:before_navigate';

/** Every destination transition asks the current page before unmounting it. */
export function request_workspace_navigation(): boolean {
  return window.dispatchEvent(new Event(navigation_event, { cancelable: true }));
}

export function use_navigation_block(blocked: boolean, on_blocked?: () => void) {
  const current = useRef({ blocked, on_blocked });
  current.current = { blocked, on_blocked };
  useEffect(() => {
    const before_navigate = (event: Event) => {
      if (!current.current.blocked) return;
      event.preventDefault();
      current.current.on_blocked?.();
    };
    const before_unload = (event: BeforeUnloadEvent) => {
      if (!current.current.blocked) return;
      event.preventDefault();
      event.returnValue = '';
    };
    window.addEventListener(navigation_event, before_navigate);
    window.addEventListener('beforeunload', before_unload);
    return () => {
      window.removeEventListener(navigation_event, before_navigate);
      window.removeEventListener('beforeunload', before_unload);
    };
  }, []);
}
