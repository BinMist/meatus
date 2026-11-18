# Meatus 管理面板（web）

Vue 3 + TypeScript + Vite 8，组件统一使用 `@sailonix/ui` 子路径按需引入，主题走 `@sailonix/ui/tokens` 与 `var(--sl-*)` 语义 token。

## 目录约定

```
web/
├─ src/
│  ├─ main.ts                 # 应用入口：tokens、组件样式、router
│  ├─ App.vue                 # 壳层：登录态、侧栏、header、footer
│  ├─ router/index.ts         # 路由表，指向 pages/<domain>/index.vue
│  ├─ pages/<domain>/         # 路由级页面，一个业务域一个目录
│  │  └─ index.vue
│  ├─ services/api.ts         # fetch 封装、session、API 调用
│  ├─ types/index.ts          # API / 页面共享类型
│  └─ styles/app.scss         # 全局基础样式与布局兜底
├─ index.html
├─ vite.config.ts
└─ package.json
```

约定：

- 一个业务域一个 `pages/<domain>/`，入口固定 `index.vue`，路由不要直接指向 `pages` 下的散文件。
- 页面私有的组件放 `pages/<domain>/components/`；跨页面复用的业务组件再提升到 `src/components/`，不要重复复制。
- API 请求集中在 `services/api.ts`；共享类型集中在 `types/index.ts`。
- 页面局部样式优先使用 `var(--sl-*)`，不要写裸元素选择器或全局类名。
- 不手改 `node_modules/@sailonix/*` 产物；定制通过 props、插槽、主题 token 与 CSS 变量完成。

## 开发与构建

```bash
npm ci
npm run dev       # 默认 5173，/api 代理到 127.0.0.1:7400
npm run build     # vue-tsc 类型检查 + Vite 构建 dist/
```

`web/dist` 由 Rust `rust-embed` 编进 `meatus-server`；改完面板要重新执行仓库根的 `cargo build --release --bin meatus` 才会更新服务器二进制中的静态资源。
