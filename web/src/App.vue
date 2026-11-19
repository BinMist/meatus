<template>
  <sl-config-provider>
    <NotificationHost />
    <router-view v-if="route.meta.public" />
    <template v-else>
      <MeatusLoading v-if="!session.ready" fullscreen size="lg" label="正在连接 meatus server" />
      <AppLayout
        v-else-if="session.authenticated"
        v-model:collapsed="railCollapsed"
        v-model:mobileOpen="mobileOpen"
        :groups="navGroups"
        :active-path="route.path"
        :group-label="groupLabel"
        :page-title="pageTitle"
        :mode="sessionMode"
        :can-logout="session.mode !== 'loopback'"
        :sidebar-width="232"
        :collapsed-width="64"
        @navigate="go"
        @logout="onLogout"
      >
        <router-view />
      </AppLayout>
    </template>
  </sl-config-provider>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { SlConfigProvider } from '@sailonix/ui/config-provider'
import AppLayout from './layout/index.vue'
import MeatusLoading from './components/MeatusLoading.vue'
import NotificationHost from './components/NotificationHost.vue'
import type { NavGroup, SessionMode } from './layout/types'
import { logout, refreshSession, session } from './services/api'

const route = useRoute()
const router = useRouter()
const mobileOpen = ref(false)
const railCollapsed = ref(false)

try {
  railCollapsed.value = localStorage.getItem('meatus.rail-collapsed') === '1'
} catch {
  /* 忽略存储不可用 */
}

watch(railCollapsed, (value) => {
  try {
    localStorage.setItem('meatus.rail-collapsed', value ? '1' : '0')
  } catch {
    /* 忽略存储不可用 */
  }
})

const navGroups: NavGroup[] = [
  {
    label: '监控',
    items: [
      { id: '/', label: '总览', icon: 'grid' },
      { id: '/proxies', label: 'Proxy 管理', icon: 'route' },
    ],
  },
  {
    label: '运维',
    items: [
      { id: '/clients', label: '客户端', icon: 'users' },
      { id: '/certificates', label: '证书状态', icon: 'shield-check' },
      { id: '/audit', label: '审计日志', icon: 'history' },
      { id: '/logs', label: '实时日志', icon: 'terminal' },
      { id: '/settings', label: '系统设置', icon: 'settings' },
    ],
  },
]

const groupLabel = computed(
  () =>
    navGroups.find((group) => group.items.some((item) => item.id === route.path))?.label ??
    '控制面',
)

const pageTitle = computed(() => String(route.meta.title ?? '控制面'))
const sessionMode = computed<SessionMode>(() =>
  session.mode === 'loopback' ? 'loopback' : 'session',
)

function go(path: string) {
  if (path !== route.path) void router.push(path)
}

onMounted(async () => {
  try {
    await refreshSession()
  } catch {
    session.authenticated = false
    session.ready = true
  }
  if (!session.authenticated && !route.meta.public) {
    await router.replace('/login')
  }
})

watch(
  () => route.fullPath,
  async () => {
    mobileOpen.value = false
    if (!session.ready) return
    if (!session.authenticated && !route.meta.public) {
      await router.replace('/login')
    }
    if (session.authenticated && route.meta.public) {
      await router.replace('/')
    }
  },
)

async function onLogout() {
  try {
    await logout()
  } finally {
    await router.replace('/login')
  }
}
</script>
