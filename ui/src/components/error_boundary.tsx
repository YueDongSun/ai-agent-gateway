import React from 'react';
import { report_react_error } from '../lib/frontend_diagnostics.js';

interface Error_boundary_props {
  children: React.ReactNode;
}

interface Error_boundary_state {
  failed: boolean;
}

export class Error_boundary extends React.Component<
  Error_boundary_props,
  Error_boundary_state
> {
  state: Error_boundary_state = { failed: false };

  static getDerivedStateFromError(_error: Error): Error_boundary_state {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    report_react_error(error, info.componentStack ?? '');
  }

  render() {
    if (this.state.failed) {
      return (
        <div className="aag-root aag-error-wrap">
          <p className="aag-error" role="alert">
            AAG could not render this view. Restart the application and use
            Diagnostics if the problem continues.
          </p>
        </div>
      );
    }
    return this.props.children;
  }
}
