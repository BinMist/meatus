<template>
  <span class="user-chip">
    <sl-avatar class="user-avatar" :initials="initials" :alt="user" size="md" />
    <span class="user-meta">
      <span class="user-name">{{ user }}</span>
      <span class="user-role">{{ roleLabel }}</span>
    </span>
  </span>
  <sl-button v-if="canLogout" type="ghost" size="sm" @click="emit('logout')">
    退出
  </sl-button>
</template>

<script setup lang="ts">
import { SlAvatar } from '@sailonix/ui/avatar'
import { SlButton } from '@sailonix/ui/button'
import { computed } from 'vue'
import type { AdminRole } from './types'

defineOptions({ name: 'TopbarAccount' })

const props = defineProps<{ canLogout: boolean; user: string; role: AdminRole }>()
const initials = computed(() => (props.user.trim().slice(0, 1) || 'M').toUpperCase())
const roleLabel = computed(
  () =>
    ({
      owner: 'owner · 全部权限',
      operator: 'operator · 运维操作',
      viewer: 'viewer · 只读',
      auditor: 'auditor · 审计只读',
    })[props.role],
)

const emit = defineEmits<{ logout: [] }>()
</script>
