<template>
  <sl-data-table
    :columns="columns"
    :data="rows"
    row-key="domain"
    caption="域名"
    unit="个"
    empty-text="没有匹配的域名证书，调整查询条件后重试"
    :page-size="20"
    :searchable="false"
    :page-sizes="[20, 50, 100]"
    dense
  >
    <template #cell="{ column, row }">
      <CertificateStatusTag
        v-if="column.key === 'status'"
        :status="row.tagStatus as CertificateRowView['tagStatus']"
        :label="String(row.tagLabel)"
      />
      <div v-else-if="column.key === 'domain'" class="mt-cert-domain-cell">
        <CertificateDomain
          :domain="String(row.domain)"
          :meta="`${row.proxy} · ${row.issuerText}`"
        />
        <p v-if="row.errorText" class="mt-cert-error" :title="String(row.errorText)">
          续期失败：{{ row.errorText }}
        </p>
      </div>
      <CertificateExpiry
        v-else-if="column.key === 'expiresText'"
        :text="String(row.expiresText)"
        :days-text="String(row.daysText)"
        :tone="row.daysTone as CertificateRowView['daysTone']"
      />
      <span v-else-if="column.key === 'serialText'" class="mt-mono-cell">{{ row.serialText }}</span>
      <sl-tag
        v-else-if="column.key === 'type'"
        class="mt-type"
        :class="`mt-type--${row.type}`"
        size="sm"
        variant="subtle"
      >{{ row.type }}</sl-tag>
      <template v-else>{{ row[column.key] }}</template>
    </template>
  </sl-data-table>
</template>

<script setup lang="ts">
import { SlDataTable } from '@sailonix/ui/data-table'
import type { DataTableColumn } from '@sailonix/ui/data-table'
import { SlTag } from '@sailonix/ui/tag'
import CertificateDomain from './CertificateDomain.vue'
import CertificateExpiry from './CertificateExpiry.vue'
import CertificateStatusTag from './CertificateStatusTag.vue'
import type { CertificateRowView } from '../types'

defineOptions({ name: 'CertificateTable' })

defineProps<{ columns: DataTableColumn[]; rows: CertificateRowView[] }>()
</script>
