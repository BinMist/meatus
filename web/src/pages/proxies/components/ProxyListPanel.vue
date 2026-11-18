<template>
  <sl-panel
    class="mt-panel-flush mt-live-table"
    title="注册中的 proxy"
    :description="`共 ${rows.length} 条 · 每 5 秒自动刷新`"
    padding="none"
  >
    <template #actions>
      <span class="band-meta">写操作会等待客户端 ack</span>
    </template>
    <div ref="tableHost" class="mt-table-host">
      <ProxyTable
        v-if="!isCardTable"
        :columns="columns"
        :rows="rows"
        :deleting="deleting"
        @copy="emit('copy', $event)"
        @edit="emit('edit', $event)"
        @remove="emit('remove', $event)"
      />
      <ProxyCardList
        v-else
        :rows="rows"
        :deleting="deleting"
        @copy="emit('copy', $event)"
        @edit="emit('edit', $event)"
        @remove="emit('remove', $event)"
      />
    </div>
  </sl-panel>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { SlPanel } from '@sailonix/ui/panel'
import type { DataTableColumn } from '@sailonix/ui/data-table'
import { formatNumber } from '../../../services/api'
import { useElementWidth } from '../../../composables/useElementWidth'
import type { ProxyRow } from '../../../types'
import ProxyCardList from './ProxyCardList.vue'
import ProxyTable from './ProxyTable.vue'
import type { ProxyRowView } from '../types'

defineOptions({ name: 'ProxyListPanel' })

defineProps<{ rows: ProxyRowView[]; deleting: string }>()

const emit = defineEmits<{
  copy: [row: ProxyRow]
  edit: [row: ProxyRow]
  remove: [row: ProxyRow]
}>()

const tableHost = ref<HTMLElement | null>(null)
const tableWidth = useElementWidth(tableHost)
const isCardTable = computed(() => tableWidth.value > 0 && tableWidth.value < 720)

const baseColumns: DataTableColumn[] = [
  { key: 'name', header: '名称', type: 'text', sortable: true, width: 118 },
  { key: 'type', header: '类型', type: 'text', width: 88 },
  { key: 'endpoint', header: '入口', type: 'text', width: 180 },
  { key: 'local_addr', header: '本地服务', type: 'text', width: 150 },
  { key: 'domainsText', header: '域名', type: 'text', width: 170 },
  {
    key: 'active_conns',
    header: '活跃',
    type: 'number',
    align: 'end',
    width: 64,
    format: (value: unknown) => formatNumber(Number(value)),
  },
  {
    key: 'conn_total',
    header: '累计连接',
    type: 'number',
    align: 'end',
    width: 82,
    format: (value: unknown) => formatNumber(Number(value)),
  },
  { key: 'traffic', header: '流量', type: 'text', align: 'end', width: 86 },
  { key: 'actions', header: '操作', type: 'actions', width: 120 },
]

const columns = computed(() => {
  const width = tableWidth.value
  if (width >= 1050) return baseColumns
  const compact = baseColumns.filter(
    (column) => !['domainsText', 'conn_total'].includes(column.key),
  )
  if (width >= 780) return compact
  return compact.filter((column) => column.key !== 'active_conns')
})
</script>
