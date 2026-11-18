<template>
  <section class="dash-side">
    <div class="dash-side-head">
      <sl-section-label class="mt-section-label" label="运行读数">
        <span class="band-meta">{{ sampling }}</span>
      </sl-section-label>
    </div>
    <div class="metric-grid">
      <sl-metric
        v-for="item in stats"
        :key="item.label"
        class="metric"
        :label="item.label"
        :value="item.value"
        :hint="item.caption"
      />
    </div>
    <div class="dash-side-stack">
      <sl-spec-plate class="spec-plate" :items="specItems" :columns="1" />
      <sl-descriptions
        class="dash-facts"
        :items="facts"
        :column="1"
        layout="horizontal"
        size="sm"
        :colon="false"
        label-width="auto"
      />
    </div>
  </section>
</template>

<script setup lang="ts">
import { SlDescriptions } from '@sailonix/ui/descriptions'
import { SlMetric } from '@sailonix/ui/metric'
import { SlSectionLabel } from '@sailonix/ui/section-label'
import { SlSpecPlate } from '@sailonix/ui/spec-plate'

defineOptions({ name: 'OverviewRuntimePanel' })

withDefaults(
  defineProps<{
    stats: Array<{ label: string; value: string | number; caption: string }>
    specItems: Array<{ key: string; label: string; value: string }>
    facts: Array<{ key: string; label: string; value: string | number }>
    sampling?: string
  }>(),
  { sampling: '5s 采样' },
)
</script>
