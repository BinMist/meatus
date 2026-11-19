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
        :user="session.user"
        :role="session.role || 'owner'"
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

const allNavGroups: NavGroup[] = [
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
      { id: '/api-tokens', label: 'API Token', icon: 'shield-check' },
      { id: '/logs', label: '实时日志', icon: 'terminal' },
      { id: '/settings', label: '系统设置', icon: 'settings' },
    ],
  },
]

const rolePaths: Record<string, string[]> = {
  owner: allNavGroups.flatMap((group) => group.items.map((item) => item.id)),
  operator: ['/', '/proxies', '/clients', '/certificates', '/logs'],
  viewer: ['/', '/proxies', '/clients', '/certificates', '/logs'],
  auditor: ['/audit'],
}

const navGroups = computed<NavGroup[]>(() => {
  const allowed = rolePaths[session.role || 'owner'] ?? rolePaths.owner
  return allNavGroups
    .map((group) => ({
      ...group,
      items: group.items.filter((item) => allowed.includes(item.id)),
    }))
    .filter((group) => group.items.length > 0)
})

const defaultPath = computed(() => navGroups.value[0]?.items[0]?.id ?? '/')
const groupLabel = computed(
  () =>
    navGroups.value.find((group) => group.items.some((item) => item.id === route.path))
      ?.label ?? '控制面',
)

function routeAllowed(path: string): boolean {
  return navGroups.value.some((group) => group.items.some((item) => item.id === path))
}

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
  } else if (session.authenticated && (!routeAllowed(route.path) || route.meta.public)) {
    await router.replace(defaultPath.value)
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
      await router.replace(defaultPath.value)
    } else if (session.authenticated && !routeAllowed(route.path)) {
      await router.replace(defaultPath.value)
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
