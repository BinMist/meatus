<template>
  <sl-status-tag
    class="mode-chip"
    :status="mode === 'loopback' ? 'success' : 'processing'"
    size="sm"
    :label="mode === 'loopback' ? 'LOOPBACK' : 'SESSION'"
    pulse
  />
</template>

<script setup lang="ts">
import { SlStatusTag } from '@sailonix/ui/status-tag'
import type { SessionMode } from './types'

defineOptions({ name: 'ModeChip' })

defineProps<{ mode: SessionMode }>()
</script>
