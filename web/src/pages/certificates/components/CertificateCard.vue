<template>
  <article class="mt-cert-card">
    <header class="mt-cert-card__head">
      <CertificateDomain :domain="row.domain" :meta="`${row.proxy} · ${row.issuerText}`" />
      <CertificateStatusTag :status="row.tagStatus" :label="row.tagLabel" />
    </header>
    <dl class="mt-cert-card__facts">
      <div>
        <dt>类型 / 来源</dt>
        <dd>{{ row.type }} · {{ row.sourceText }}</dd>
      </div>
      <div>
        <dt>到期 / 剩余</dt>
        <dd class="mt-cert-card__expiry">
          <span>{{ row.expiresText }}</span>
          <b :class="`is-${row.daysTone}`">{{ row.daysText }}</b>
        </dd>
      </div>
      <div>
        <dt>证书链</dt>
        <dd>{{ row.chainText }}</dd>
      </div>
    </dl>
  </article>
</template>

<script setup lang="ts">
import CertificateDomain from './CertificateDomain.vue'
import CertificateStatusTag from './CertificateStatusTag.vue'
import type { CertificateRowView } from '../types'

defineOptions({ name: 'CertificateCard' })

defineProps<{ row: CertificateRowView }>()
</script>
