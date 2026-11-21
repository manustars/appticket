# web

The agents' UI: a React application, built with Vite, that talks to the core only through its REST
API.

```sh
npm install
npm run dev     # dev server; /api is proxied to the core at http://127.0.0.1:8080
npm run build   # type-check and build into dist/
npm run lint
```

Dependency versions are pinned exactly (`.npmrc` sets `save-exact`).
