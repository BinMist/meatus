<template>
  <sl-panel
    class="mt-panel-flush mt-live-table"
    title="已连接客户端"
    :description="`共 ${rows.length} 条 · TLS + yamux 控制会话`"
    padding="none"
  >
    <template #actions>
      <span class="band-meta">{{ updatedAt ? `更新于 ${updatedAt}` : '等待采样' }}</span>
      <sl-button type="secondary" size="sm" @click="emit('refresh')">刷新</sl-button>
    </template>
    <sl-data-table
      :columns="clientColumns"
      :data="rows"
      row-key="run_id"
      caption="客户端"
      unit="个"
      empty-text="当前没有客户端连接"
      :page-size="50"
      :searchable="false"
      :page-sizes="[50, 100]"
      dense
    >
      <template #cell="{ column, row }">
        <span v-if="column.key === 'client_id'" class="mt-client-name">
          <i class="mt-client-dot" />
          {{ row.client_id }}
        </span>
        <span v-else-if="column.key === 'run_id'" class="mt-mono-cell">{{ row.run_id }}</span>
        <span v-else-if="column.key === 'connected'" class="mt-mono-cell">{{ row.connected }}</span>
        <span v-else-if="column.key === 'heartbeat'" class="mt-mono-cell">{{ row.heartbeat }}</span>
        <span v-else-if="column.key === 'proxies'" class="mt-mono-cell">{{ row.proxiesText }}</span>
        <template v-else>{{ row[column.key] }}</template>
      </template>
    </sl-data-table>
  </sl-panel>
</template>

<script setup lang="ts">
import { SlButton } from '@sailonix/ui/button'
import { SlDataTable } from '@sailonix/ui/data-table'
import { SlPanel } from '@sailonix/ui/panel'

defineOptions({ name: 'OverviewClientsPanel' })

defineProps<{
  rows: Array<Record<string, unknown> & { run_id: string }>
  updatedAt: string
}>()

const emit = defineEmits<{ refresh: [] }>()

const clientColumns = [
  { key: 'client_id', header: '客户端', type: 'text' as const },
  { key: 'tenant_id', header: '租户', type: 'text' as const },
  { key: 'proxies', header: 'Proxy', type: 'text' as const },
  { key: 'traffic', header: '流量', type: 'text' as const, align: 'end' as const, width: 100 },
  { key: 'heartbeat', header: '最后心跳', type: 'text' as const, width: 110 },
  { key: 'connected', header: '连接时间', type: 'text' as const, width: 120 },
  { key: 'run_id', header: 'Run ID', type: 'text' as const, width: 140 },
]
</script>
