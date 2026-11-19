<template>
  <article class="mt-proxy-card">
    <header class="mt-proxy-card__head">
      <b class="mt-proxy-card__name">{{ row.name }}</b>
      <ProxyTypeTag :type="row.type" />
    </header>
    <div class="mt-proxy-card__endpoint">{{ row.endpoint }}</div>
    <dl class="mt-proxy-card__facts">
      <div>
        <dt>本地服务</dt>
        <dd class="mt-mono-cell">{{ row.local_addr }}</dd>
      </div>
      <div v-if="row.domainsText">
        <dt>域名</dt>
        <dd class="mt-domain-cell">{{ row.domainsText }}</dd>
      </div>
      <div v-if="row.probeText">
        <dt>诊断</dt>
        <dd class="mt-probe-line" :class="`is-${row.probeTone}`">{{ row.probeText }}</dd>
      </div>
      <div>
        <dt>活跃 / 累计</dt>
        <dd>{{ formatNumber(Number(row.active_conns)) }} / {{ formatNumber(Number(row.conn_total)) }}</dd>
      </div>
      <div>
        <dt>流量</dt>
        <dd class="mt-mono-cell">{{ row.traffic }}</dd>
      </div>
    </dl>
    <footer class="mt-proxy-card__foot">
      <ProxyRowActions
        :row="row"
        :deleting="deleting"
        :probing="probing"
        @copy="emit('copy', $event)"
        @edit="emit('edit', $event)"
        @remove="emit('remove', $event)"
        @probe="emit('probe', $event)"
      />
    </footer>
  </article>
</template>

<script setup lang="ts">
import { formatNumber } from '../../../services/api'
import type { ProxyRow } from '../../../types'
import ProxyRowActions from '../ProxyRowActions.vue'
import ProxyTypeTag from './ProxyTypeTag.vue'
import type { ProxyRowView } from '../types'

defineOptions({ name: 'ProxyCard' })

defineProps<{ row: ProxyRowView; deleting: string; probing: string }>()

const emit = defineEmits<{
  copy: [row: ProxyRow]
  edit: [row: ProxyRow]
  remove: [row: ProxyRow]
  probe: [row: ProxyRow]
}>()
</script>
