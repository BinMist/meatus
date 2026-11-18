<template>
  <section class="band">
    <div class="band-head">
      <sl-section-label class="mt-section-label" label="概览">
        <sl-space class="band-tools" size="md" wrap>
          <sl-segmented-control v-model="levelModel" size="sm" aria-label="日志级别筛选">
            <sl-segmented-control-item value="all" label="全部" />
            <sl-segmented-control-item value="info" label="INFO" />
            <sl-segmented-control-item value="warn" label="WARN" />
            <sl-segmented-control-item value="error" label="ERROR" />
          </sl-segmented-control>
          <sl-switch v-model="autoScrollModel" label="自动滚动" size="sm" />
        </sl-space>
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
        :tone="item.tone"
      />
    </sl-readout-strip>
  </section>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { SlMetric } from '@sailonix/ui/metric'
import { SlReadoutStrip } from '@sailonix/ui/readout-strip'
import { SlSectionLabel } from '@sailonix/ui/section-label'
import {
  SlSegmentedControl,
  SlSegmentedControlItem,
} from '@sailonix/ui/segmented-control'
import { SlSpace } from '@sailonix/ui/space'
import { SlSwitch } from '@sailonix/ui/switch'

defineOptions({ name: 'LogsSummary' })

type LogLevel = 'all' | 'info' | 'warn' | 'error'
type SummaryTone = 'default' | 'success' | 'danger' | 'muted'

const props = defineProps<{
  stats: Array<{
    label: string
    value: string | number
    unit?: string
    hint: string
    tone: SummaryTone
  }>
  level: LogLevel
  autoScroll: boolean
}>()

const emit = defineEmits<{
  'update:level': [value: LogLevel]
  'update:autoScroll': [value: boolean]
}>()

const levelModel = computed({
  get: () => props.level,
  set: (value: LogLevel) => emit('update:level', value),
})
const autoScrollModel = computed({
  get: () => props.autoScroll,
  set: (value: boolean) => emit('update:autoScroll', value),
})
</script>
