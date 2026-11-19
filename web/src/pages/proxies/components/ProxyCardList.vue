<template>
  <div class="mt-proxy-cards">
    <ProxyCard
      v-for="row in rows"
      :key="row.key"
      :row="row"
      :deleting="deleting"
      :probing="probing"
      @copy="emit('copy', $event)"
      @edit="emit('edit', $event)"
      @remove="emit('remove', $event)"
      @probe="emit('probe', $event)"
    />
    <p v-if="!rows.length" class="mt-proxy-cards__empty">
      没有匹配的 proxy，调整查询条件或点击右上角新增
    </p>
  </div>
</template>

<script setup lang="ts">
import type { ProxyRow } from '../../../types'
import ProxyCard from './ProxyCard.vue'
import type { ProxyRowView } from '../types'

defineOptions({ name: 'ProxyCardList' })

defineProps<{ rows: ProxyRowView[]; deleting: string; probing: string }>()

const emit = defineEmits<{
  copy: [row: ProxyRow]
  edit: [row: ProxyRow]
  remove: [row: ProxyRow]
  probe: [row: ProxyRow]
}>()
</script>
