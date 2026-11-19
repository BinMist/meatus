<template>
  <sl-data-table
    :columns="columns"
    :data="rows"
    row-key="key"
    caption="Proxy"
    unit="个"
    empty-text="没有匹配的 proxy，调整查询条件或点击右上角新增"
    :page-size="20"
    :searchable="false"
    :page-sizes="[20, 50, 100]"
    dense
  >
    <template #cell="{ column, row }">
      <ProxyTypeTag v-if="column.key === 'type'" :type="String(row.type)" />
      <span v-else-if="column.key === 'endpoint'" class="mt-mono-cell">{{ row.endpoint }}</span>
      <div v-else-if="column.key === 'local_addr'" class="mt-local-cell">
        <span class="mt-mono-cell">{{ row.local_addr }}</span>
        <span
          v-if="row.probeText"
          class="mt-probe-line"
          :class="`is-${row.probeTone}`"
          :title="String(row.probeText)"
        >{{ row.probeText }}</span>
      </div>
      <span
        v-else-if="column.key === 'domainsText'"
        class="mt-domain-cell"
        :title="String(row.domainsText ?? '')"
      >{{ row.domainsText }}</span>
      <span v-else-if="column.key === 'conn_total'" class="mt-mono-cell">{{
        formatNumber(Number(row.conn_total))
      }}</span>
      <span v-else-if="column.key === 'traffic'" class="mt-mono-cell">{{ row.traffic }}</span>
      <template v-else>{{ row[column.key] }}</template>
    </template>
    <template #actions="{ row }">
      <ProxyRowActions
        :row="asProxy(row)"
        :deleting="deleting"
        :probing="probing"
        @copy="emit('copy', $event)"
        @edit="emit('edit', $event)"
        @remove="emit('remove', $event)"
        @probe="emit('probe', $event)"
      />
    </template>
  </sl-data-table>
</template>

<script setup lang="ts">
import { SlDataTable } from '@sailonix/ui/data-table'
import type { DataTableColumn, DataTableRow } from '@sailonix/ui/data-table'
import { formatNumber } from '../../../services/api'
import type { ProxyRow } from '../../../types'
import ProxyRowActions from '../ProxyRowActions.vue'
import ProxyTypeTag from './ProxyTypeTag.vue'
import type { ProxyRowView } from '../types'

defineOptions({ name: 'ProxyTable' })

defineProps<{
  columns: DataTableColumn[]
  rows: ProxyRowView[]
  deleting: string
  probing: string
}>()

const emit = defineEmits<{
  copy: [row: ProxyRow]
  edit: [row: ProxyRow]
  remove: [row: ProxyRow]
  probe: [row: ProxyRow]
}>()

function asProxy(row: DataTableRow): ProxyRowView {
  return row as unknown as ProxyRowView
}
</script>
