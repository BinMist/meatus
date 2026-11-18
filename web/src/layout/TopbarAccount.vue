<template>
  <span class="user-chip">
    <sl-avatar class="user-avatar" initials="M" alt="本机管理员" size="md" />
    <span class="user-meta">
      <span class="user-name">本机管理员</span>
      <span class="user-role">meatus control plane</span>
    </span>
  </span>
  <sl-button v-if="canLogout" type="ghost" size="sm" @click="emit('logout')">
    退出
  </sl-button>
</template>

<script setup lang="ts">
import { SlAvatar } from '@sailonix/ui/avatar'
import { SlButton } from '@sailonix/ui/button'

defineOptions({ name: 'TopbarAccount' })

defineProps<{ canLogout: boolean }>()

const emit = defineEmits<{ logout: [] }>()
</script>
