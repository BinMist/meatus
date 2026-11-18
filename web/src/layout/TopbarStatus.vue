<template>
  <span class="topbar-bind">{{ bindLabel }}</span>
  <ModeChip :mode="mode" />
</template>

<script setup lang="ts">
import ModeChip from './ModeChip.vue'
import type { SessionMode } from './types'

defineOptions({ name: 'TopbarStatus' })

withDefaults(
  defineProps<{
    mode: SessionMode
    bindLabel?: string
  }>(),
  { bindLabel: '127.0.0.1 · admin api' },
)
</script>
