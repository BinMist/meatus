<template>
  <sl-panel
    class="mt-panel-flush"
    title="域名与证书"
    :description="`共 ${rows.length} 条 · 每 10 秒刷新`"
    padding="none"
  >
    <template #actions>
      <span class="band-meta">证书元数据来自 cert_dir 中的 PEM 解析</span>
    </template>
    <div ref="tableHost" class="mt-table-host">
      <CertificateTable v-if="!isCardTable" :columns="columns" :rows="rows" />
      <CertificateCardList v-else :rows="rows" />
    </div>
  </sl-panel>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { SlPanel } from '@sailonix/ui/panel'
import type { DataTableColumn } from '@sailonix/ui/data-table'
import { useElementWidth } from '../../../composables/useElementWidth'
import CertificateCardList from './CertificateCardList.vue'
import CertificateTable from './CertificateTable.vue'
import type { CertificateRowView } from '../types'

defineOptions({ name: 'CertificateListPanel' })

defineProps<{ rows: CertificateRowView[] }>()

const tableHost = ref<HTMLElement | null>(null)
const tableWidth = useElementWidth(tableHost)
const isCardTable = computed(() => tableWidth.value > 0 && tableWidth.value < 520)

const baseColumns: DataTableColumn[] = [
  { key: 'domain', header: '域名 / Proxy', type: 'text', sortable: true, width: 220 },
  { key: 'type', header: '类型', type: 'text', width: 74 },
  { key: 'sourceText', header: '来源', type: 'text', width: 92 },
  { key: 'status', header: '状态', type: 'text', width: 106 },
  { key: 'chainText', header: '证书链', type: 'text', width: 76 },
  { key: 'expiresText', header: '到期 / 剩余', type: 'text', width: 180 },
  { key: 'serialText', header: '序列号', type: 'text', width: 170 },
]

const columns = computed(() => {
  const width = tableWidth.value
  if (width >= 920) return baseColumns
  let compact = baseColumns.filter((column) => column.key !== 'serialText')
  if (width < 700) compact = compact.filter((column) => column.key !== 'sourceText')
  if (width < 620) compact = compact.filter((column) => column.key !== 'chainText')
  if (width < 580) compact = compact.filter((column) => column.key !== 'type')
  return compact
})
</script>
