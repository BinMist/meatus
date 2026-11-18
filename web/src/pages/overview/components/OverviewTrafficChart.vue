<template>
  <sl-line-chart-card
    class="overview-traffic"
    :class="full ? 'dash-chart dash-chart--full' : 'dash-hero'"
    :title="title"
    :values="values"
    :labels="labels"
    :headline="headline"
    :suffix="` ${unit}`"
    shape="smooth"
    :height="height"
    area
  />
</template>

<script setup lang="ts">
import { SlLineChartCard } from '@sailonix/ui/line-chart-card'

defineOptions({ name: 'OverviewTrafficChart' })

withDefaults(
  defineProps<{
    title: string
    values: number[]
    labels: string[]
    headline: number
    unit: string
    height?: number
    full?: boolean
  }>(),
  { height: 212, full: false },
)
</script>
