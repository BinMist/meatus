<template>
  <div class="rail-foot">
    <div class="rail-foot-line">
      <sl-status-tag class="rail-live" status="success" size="sm" label="在线" pulse />
      <span class="rail-foot-mono">{{ version }}</span>
    </div>
    <div class="rail-foot-note">
      {{ mode === 'loopback' ? 'loopback · 本地免登录' : `${user || 'session'} · ${role}` }}
    </div>
  </div>
</template>

<script setup lang="ts">
import { SlStatusTag } from '@sailonix/ui/status-tag'
import type { AdminRole, SessionMode } from './types'
import { APP_VERSION_TAG } from '../version'

defineOptions({ name: 'SidebarFooter' })

withDefaults(
  defineProps<{
    mode: SessionMode
    user?: string
    role?: AdminRole
    version?: string
  }>(),
  { user: 'admin', role: 'owner', version: APP_VERSION_TAG },
)
</script>
