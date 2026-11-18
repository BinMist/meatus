<template>
  <sl-tag class="mt-type" :class="`mt-type--${type}`" size="sm" variant="subtle">
    {{ type }}
  </sl-tag>
</template>

<script setup lang="ts">
import { SlTag } from '@sailonix/ui/tag'

defineOptions({ name: 'ProxyTypeTag' })

defineProps<{ type: string }>()
</script>
