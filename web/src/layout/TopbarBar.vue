<template>
  <div class="topbar">
    <TopbarMenuButton :collapsed="collapsed" @toggle="emit('toggle')" />
    <TopbarCrumbs :group-label="groupLabel" :page-title="pageTitle" />
    <span class="spacer" />
    <TopbarStatus :mode="mode" :bind-label="bindLabel" />
    <TopbarAccount :can-logout="canLogout" @logout="emit('logout')" />
  </div>
</template>

<script setup lang="ts">
import TopbarAccount from './TopbarAccount.vue'
import TopbarCrumbs from './TopbarCrumbs.vue'
import TopbarMenuButton from './TopbarMenuButton.vue'
import TopbarStatus from './TopbarStatus.vue'
import type { SessionMode } from './types'

defineOptions({ name: 'TopbarBar' })

withDefaults(
  defineProps<{
    collapsed: boolean
    groupLabel: string
    pageTitle: string
    mode: SessionMode
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
