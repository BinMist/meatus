<template>
  <div class="mt-cert-cards">
    <CertificateCard v-for="row in rows" :key="String(row.domain)" :row="row" />
    <p v-if="!rows.length" class="mt-cert-cards__empty">
      没有匹配的域名证书，调整查询条件后重试
    </p>
  </div>
</template>

<script setup lang="ts">
import CertificateCard from './CertificateCard.vue'
import type { CertificateRowView } from '../types'

defineOptions({ name: 'CertificateCardList' })

defineProps<{ rows: CertificateRowView[] }>()
</script>
