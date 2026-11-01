# Client model pages

`AppPage.tsx` renders only configured router model selections for the current
client. It receives a model-only projection through `lib/api/client_router_plan.ts`.
Loading, empty and failure states do not fall back to global catalogs.

This directory contains no connection inventory, credentials, source metadata,
management forms, routing editor or launcher state. Those retained operations
belong to `components/suppliers/client_management/`, mounted only from Suppliers.

`client_plan.css` reuses the shared design tokens. Navigation metadata comes from
`lib/client_apps.ts`; no connection-form configuration enters the page or shell.
