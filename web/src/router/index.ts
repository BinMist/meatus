import { createRouter, createWebHistory } from 'vue-router'

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/login', name: 'login', component: () => import('../pages/login/index.vue'), meta: { public: true } },
    { path: '/', name: 'overview', component: () => import('../pages/overview/index.vue'), meta: { title: '总览', icon: 'dashboard' } },
    { path: '/proxies', name: 'proxies', component: () => import('../pages/proxies/index.vue'), meta: { title: 'Proxy 管理', icon: 'server' } },
    { path: '/clients', name: 'clients', component: () => import('../pages/clients/index.vue'), meta: { title: '客户端与令牌', icon: 'users' } },
    { path: '/certificates', name: 'certificates', component: () => import('../pages/certificates/index.vue'), meta: { title: '证书状态', icon: 'shield' } },
    { path: '/audit', name: 'audit', component: () => import('../pages/audit/index.vue'), meta: { title: '审计日志', icon: 'history' } },
    { path: '/api-tokens', name: 'api-tokens', component: () => import('../pages/api-tokens/index.vue'), meta: { title: 'API Token', icon: 'shield' } },
    { path: '/logs', name: 'logs', component: () => import('../pages/logs/index.vue'), meta: { title: '实时日志', icon: 'terminal' } },
    { path: '/settings', name: 'settings', component: () => import('../pages/settings/index.vue'), meta: { title: '系统设置', icon: 'settings' } },
    { path: '/:pathMatch(.*)*', redirect: '/' },
  ],
})
