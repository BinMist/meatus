<template>
  <div class="app-stage">
    <sl-app-shell
      class="mt-frame"
      :title="title"
      :sidebar-width="sidebarWidth"
      :collapsed-width="collapsedWidth"
      :collapsible="true"
      :collapsed="collapsed"
      :mobile-open="mobileOpen"
      nav-label="主导航"
      @update:collapsed="emit('update:collapsed', $event)"
      @update:mobileOpen="emit('update:mobileOpen', $event)"
    >
      <template #brand>
        <SidebarBrand />
      </template>

      <template #sidebar>
        <SidebarNav :groups="groups" :active-path="activePath" @navigate="handleNavigate" />
      </template>

      <template #sidebar-footer>
        <SidebarFooter :mode="mode" :user="user" :role="role" />
      </template>

      <template #header>
        <TopbarBar
          :collapsed="collapsed"
          :group-label="groupLabel"
          :page-title="pageTitle"
          :mode="mode"
          :user="user"
          :role="role"
          :can-logout="canLogout"
          @toggle="toggleNavigation"
          @logout="emit('logout')"
        />
      </template>

      <slot />
    </sl-app-shell>
  </div>
</template>

<script setup lang="ts">
import { SlAppShell } from '@sailonix/ui/app-shell'
import SidebarBrand from './SidebarBrand.vue'
import SidebarFooter from './SidebarFooter.vue'
import SidebarNav from './SidebarNav.vue'
import TopbarBar from './TopbarBar.vue'
import type { AdminRole, NavGroup, SessionMode } from './types'

defineOptions({ name: 'AppLayout' })

const props = withDefaults(
  defineProps<{
    title?: string
    groups: NavGroup[]
    activePath: string
    groupLabel: string
    pageTitle: string
    mode: SessionMode
    user?: string
    role?: AdminRole
    canLogout?: boolean
    sidebarWidth?: number
    collapsedWidth?: number
    collapsed?: boolean
    mobileOpen?: boolean
  }>(),
  {
    title: 'Meatus',
    user: 'admin',
    role: 'owner',
    canLogout: false,
    sidebarWidth: 232,
    collapsedWidth: 64,
    collapsed: false,
    mobileOpen: false,
  },
)

const emit = defineEmits<{
  'update:collapsed': [value: boolean]
  'update:mobileOpen': [value: boolean]
  navigate: [path: string]
  logout: []
}>()

function handleNavigate(path: string) {
  emit('update:mobileOpen', false)
  emit('navigate', path)
}

function toggleNavigation() {
  if (window.matchMedia('(max-width: 900px)').matches) {
    emit('update:mobileOpen', true)
    return
  }
  emit('update:collapsed', !props.collapsed)
}
</script>
