<template>
  <sl-icon-button
    class="topbar-menu"
    :class="{ 'is-collapsed': collapsed }"
    type="ghost"
    size="md"
    :aria-label="collapsed ? '展开导航' : '收起导航'"
    :aria-expanded="!collapsed"
    @click="emit('toggle')"
  >
    <svg class="topbar-menu-ico" viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
      <path d="M9 4.5v15" />
      <path v-if="collapsed" d="M11.5 9.5 14 12l-2.5 2.5" />
      <path v-else d="M14 9.5 11.5 12l2.5 2.5" />
    </svg>
  </sl-icon-button>
</template>

<script setup lang="ts">
import { SlIconButton } from '@sailonix/ui/icon-button'

defineOptions({ name: 'TopbarMenuButton' })

defineProps<{ collapsed: boolean }>()

const emit = defineEmits<{ toggle: [] }>()
</script>
