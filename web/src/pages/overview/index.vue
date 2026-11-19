<template>
  <div class="page dashboard">
    <sl-page-header title="总览" sub-title="relay 运行状态、流量与已连接客户端">
      <template #extra>
        <PageActionsBar :meta="updatedAt ? `采样于 ${updatedAt}` : '等待采样'" @refresh="refresh">
          <template #prefix>
            <sl-status-tag
              :status="error ? 'error' : 'success'"
              :label="error ? '连接异常' : '运行中'"
              :pulse="!error"
            />
          </template>
        </PageActionsBar>
      </template>
    </sl-page-header>

    <div class="dash-grid">
      <OverviewTrafficChart
        title="累计入站流量"
        :values="history"
        :labels="historyLabels"
        :headline="trafficIn.value"
        :unit="trafficIn.unit"
        :height="212"
      />
      <OverviewRuntimePanel :stats="stats" :spec-items="specItems" :facts="dashFacts" />
      <OverviewTrafficChart
        title="累计出站流量"
        :values="historyOut"
        :labels="historyLabels"
        :headline="trafficOut.value"
        :unit="trafficOut.unit"
        :height="180"
        full
      />
    </div>

    <OverviewClientsPanel :rows="clientRows" :updated-at="updatedAt" @refresh="refresh" />
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { SlPageHeader } from '@sailonix/ui/page-header'
import { SlStatusTag } from '@sailonix/ui/status-tag'
import PageActionsBar from '../../components/PageActionsBar.vue'
import OverviewClientsPanel from './components/OverviewClientsPanel.vue'
import OverviewRuntimePanel from './components/OverviewRuntimePanel.vue'
import OverviewTrafficChart from './components/OverviewTrafficChart.vue'
import { notifyError } from '../../services/notifications'
import { apiGet, formatNumber, relativeTime, session } from '../../services/api'
import type { StatusSnapshot } from '../../types'
import { APP_VERSION } from '../../version'

const snapshot = ref<StatusSnapshot | null>(null)
const error = ref('')
const updatedAt = ref('')
const history = ref<number[]>([0])
const historyOut = ref<number[]>([0])
const historyLabels = ref<string[]>(['—'])
let timer: number | undefined

const totalBytesIn = computed(() => snapshot.value?.totals.bytes_in ?? 0)
const totalBytesOut = computed(() => snapshot.value?.totals.bytes_out ?? 0)

function scaleBytes(bytes: number) {
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let index = 0
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024
    index += 1
  }
  return {
    value: index === 0 ? value : Number(value.toFixed(2)),
    unit: units[index],
  }
}

const trafficIn = computed(() => scaleBytes(totalBytesIn.value))
const trafficOut = computed(() => scaleBytes(totalBytesOut.value))

const uptimeText = computed(() => {
  const total = snapshot.value?.uptime_s ?? 0
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  return [hours, minutes, seconds].map((value) => String(value).padStart(2, '0')).join(':')
})

const stats = computed(() => [
  {
    label: '在线客户端',
    value: formatNumber(snapshot.value?.totals.clients ?? 0),
    caption: '控制面会话',
  },
  {
    label: '活跃 proxy',
    value: formatNumber(snapshot.value?.totals.proxies ?? 0),
    caption: '已注册路由',
  },
  {
    label: '活跃连接',
    value: formatNumber(snapshot.value?.totals.active_conns ?? 0),
    caption: '当前转发中',
  },
  {
    label: '运行时长',
    value: uptimeText.value,
    caption: 'server 进程',
  },
])

const specItems = computed(() => [
  { key: 'bind', label: 'bind', value: '127.0.0.1' },
  { key: 'mode', label: 'mode', value: session.mode === 'loopback' ? 'LOOPBACK' : 'SESSION' },
  { key: 'sampling', label: 'sampling', value: '5s' },
  { key: 'version', label: 'version', value: APP_VERSION },
])

const dashFacts = computed(() => [
  { key: 'clients', label: '在线客户端', value: snapshot.value?.totals.clients ?? 0 },
  { key: 'proxies', label: '活跃 proxy', value: snapshot.value?.totals.proxies ?? 0 },
  { key: 'uptime', label: '运行时长', value: uptimeText.value },
])

const clientRows = computed(() =>
  (snapshot.value?.clients ?? []).map((client) => ({
    ...client,
    connected: relativeTime(client.connected_at_ms),
    run_id: client.run_id.slice(0, 12),
  })),
)

async function refresh() {
  try {
    const data = await apiGet<StatusSnapshot>('/api/v1/status')
    snapshot.value = data
    error.value = ''
    const now = new Date()
    updatedAt.value = now.toLocaleTimeString('zh-CN', { hour12: false })
    const label = now.toLocaleTimeString('zh-CN', { hour12: false })
    history.value = [...history.value.slice(-29), data.totals.bytes_in]
    historyOut.value = [...historyOut.value.slice(-29), data.totals.bytes_out]
    historyLabels.value = [...historyLabels.value.slice(-29), label]
  } catch (err) {
    const message = err instanceof Error ? err.message : '请求失败'
    if (error.value !== message) {
      error.value = message
      notifyError('无法获取运行状态', message)
    }
  }
}

onMounted(() => {
  refresh()
  timer = window.setInterval(refresh, 5000)
})
onBeforeUnmount(() => {
  if (timer) window.clearInterval(timer)
})
</script>
