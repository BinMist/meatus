<template>
  <div class="topbar">
    <TopbarMenuButton :collapsed="collapsed" @toggle="emit('toggle')" />
    <TopbarCrumbs :group-label="groupLabel" :page-title="pageTitle" />
    <span class="spacer" />
    <TopbarStatus :mode="mode" :bind-label="bindLabel" />
    <TopbarAccount :user="user" :role="role" :can-logout="canLogout" @logout="emit('logout')" />
  </div>
</template>

<script setup lang="ts">
import TopbarAccount from './TopbarAccount.vue'
import TopbarCrumbs from './TopbarCrumbs.vue'
import TopbarMenuButton from './TopbarMenuButton.vue'
import TopbarStatus from './TopbarStatus.vue'
import type { AdminRole, SessionMode } from './types'

defineOptions({ name: 'TopbarBar' })

withDefaults(
  defineProps<{
    collapsed: boolean
    groupLabel: string
    pageTitle: string
    mode: SessionMode
    user: string
    role: AdminRole
    canLogout: boolean
    bindLabel?: string
  }>(),
  { bindLabel: '127.0.0.1 · admin api' },
)

const emit = defineEmits<{
  toggle: []
  logout: []
}>()
</script>
