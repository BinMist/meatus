<template>
  <sl-space class="page-actions" size="sm" wrap>
    <span v-if="meta" class="band-meta">{{ meta }}</span>
    <slot name="prefix" />
    <sl-button v-if="refreshable" type="secondary" size="sm" :disabled="busy" @click="emit('refresh')">
      <template #leading><sl-icon name="refresh" :size="15" /></template>
      {{ refreshLabel }}
    </sl-button>
    <slot />
  </sl-space>
</template>

<script setup lang="ts">
import { SlButton } from '@sailonix/ui/button'
import { SlIcon } from '@sailonix/icon'
import { SlSpace } from '@sailonix/ui/space'

defineOptions({ name: 'PageActionsBar' })

withDefaults(
  defineProps<{
    meta?: string
    refreshable?: boolean
    busy?: boolean
    refreshLabel?: string
  }>(),
  {
    meta: '',
    refreshable: true,
    busy: false,
    refreshLabel: '刷新',
  },
)

const emit = defineEmits<{ refresh: [] }>()
</script>
