<template>
  <div class="page page--logs">
    <sl-page-header title="实时日志" sub-title="server 进程内最近日志，不落盘、不包含 admin token">
      <template #extra>
        <PageActionsBar
          :meta="updatedAt ? `更新于 ${updatedAt}` : '等待采样'"
          @refresh="refresh"
        />
      </template>
    </sl-page-header>

    <LogsSummary
      :stats="summaryStats"
      :level="level"
      :auto-scroll="autoScroll"
      @update:level="level = $event"
      @update:auto-scroll="autoScroll = $event"
    />

    <div class="band-label">
      <sl-section-label class="mt-section-label" label="日志明细" />
    </div>

    <LogsPanel
      :entries="filtered"
      :total="total"
      :source-count="lines.length"
      :updated-at="updatedAt"
      :follow="autoScroll"
    />
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { SlPageHeader } from '@sailonix/ui/page-header'
import { SlSectionLabel } from '@sailonix/ui/section-label'
import PageActionsBar from '../../components/PageActionsBar.vue'
import LogsPanel from './components/LogsPanel.vue'
import LogsSummary from './components/LogsSummary.vue'
import { apiGet } from '../../services/api'
import type { LogsSnapshot } from '../../types'

const lines = ref<string[]>([])
const total = ref(0)
const level = ref<'all' | 'info' | 'warn' | 'error'>('all')
const autoScroll = ref(true)
const updatedAt = ref('')
let timer: number | undefined

const entries = computed(() =>
  lines.value.map((line, index) => {
    const match = line.match(/^(\S+)\s+(\w+)\s+(\S+):\s+(.*)$/)
    const iso = match?.[1] ?? ''
    return {
      no: index + 1,
      time: iso ? iso.slice(11, 23) : '',
      level: (match?.[2] ?? 'info').toLowerCase(),
      target: match?.[3] ?? '',
      message: match?.[4] ?? line,
    }
  }),
)

const counts = computed(() => ({
  warn: entries.value.filter((entry) => entry.level === 'warn').length,
  error: entries.value.filter((entry) => entry.level === 'error').length,
}))

type SummaryTone = 'default' | 'success' | 'danger' | 'muted'

const summaryStats = computed<
  Array<{ label: string; value: string | number; unit?: string; hint: string; tone: SummaryTone }>
>(() => [
  {
    label: '缓冲区',
    value: total.value,
    unit: '行',
    hint: '进程内环形缓冲，重启即清空',
    tone: 'default',
  },
  {
    label: '当前显示',
    value: filtered.value.length,
    unit: '行',
    hint: level.value === 'all' ? '全部级别' : `${level.value.toUpperCase()} 过滤中`,
    tone: 'default',
  },
  {
    label: 'WARN',
    value: counts.value.warn,
    hint: '本次缓冲窗口内',
    tone: counts.value.warn > 0 ? 'danger' : 'muted',
  },
  {
    label: 'ERROR',
    value: counts.value.error,
    hint: '需要关注的失败事件',
    tone: counts.value.error > 0 ? 'danger' : 'muted',
  },
])

const filtered = computed(() =>
  level.value === 'all'
    ? entries.value
    : entries.value.filter((entry) => entry.level === level.value),
)

async function refresh() {
  try {
    const data = await apiGet<LogsSnapshot>('/api/v1/logs?limit=300')
    lines.value = data.lines
    total.value = data.total
    updatedAt.value = new Date().toLocaleTimeString('zh-CN', { hour12: false })
  } catch {
    // 会话失效由全局登录态处理。
  }
}

onMounted(() => {
  refresh()
  timer = window.setInterval(refresh, 2000)
})
onBeforeUnmount(() => {
  if (timer) window.clearInterval(timer)
})
</script>
