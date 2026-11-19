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
import { APP_VERSION_TAG } from '../version'

defineOptions({ name: 'SidebarFooter' })

withDefaults(
  defineProps<{
    mode: SessionMode
    version?: string
  }>(),
  { version: APP_VERSION_TAG },
)
</script>
