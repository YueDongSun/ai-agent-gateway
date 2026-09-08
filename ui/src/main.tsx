import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { initDemo } from './lib/mock/backend';
import './styles.css';

// In a plain browser (no Tauri bridge) install the in-browser mock backend so
// the whole UI can be exercised under `npm run dev` without the Rust app.
initDemo();

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);