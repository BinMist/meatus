<template>
  <div class="rail-foot">
    <div class="rail-foot-line">
      <sl-status-tag class="rail-live" status="success" size="sm" label="在线" pulse />
      <span class="rail-foot-mono">{{ version }}</span>
    </div>
    <div class="rail-foot-note">
      {{ mode === 'loopback' ? 'loopback · 本地免登录' : 'session · 已认证' }}
    </div>
  </div>
</template>

<script setup lang="ts">
import { SlStatusTag } from '@sailonix/ui/status-tag'
import type { SessionMode } from './types'

defineOptions({ name: 'SidebarFooter' })

withDefaults(
  defineProps<{
    mode: SessionMode
    version?: string
  }>(),
  { version: 'v0.1.0' },
)
</script>
