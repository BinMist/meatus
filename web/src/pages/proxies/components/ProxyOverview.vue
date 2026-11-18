<template>
  <section class="band">
    <div class="band-head">
      <sl-section-label class="mt-section-label" label="概览">
        <span class="band-meta">{{ proxyCount }} 条 · {{ clientCount }} 台客户端</span>
      </sl-section-label>
    </div>
    <sl-readout-strip class="mt-readout-strip" :divider="false" wrap>
      <sl-metric
        v-for="item in stats"
        :key="item.label"
        :label="item.label"
        :value="item.value"
        :unit="item.unit"
        :hint="item.hint"
      />
    </sl-readout-strip>
  </section>
</template>

<script setup lang="ts">
import { SlMetric } from '@sailonix/ui/metric'
import { SlReadoutStrip } from '@sailonix/ui/readout-strip'
import { SlSectionLabel } from '@sailonix/ui/section-label'

defineOptions({ name: 'ProxyOverview' })

defineProps<{
  stats: Array<{ label: string; value: string | number; unit?: string; hint: string }>
  proxyCount: number
  clientCount: number
}>()
</script>
