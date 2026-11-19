<template>
  <div class="page">
    <sl-page-header title="证书状态" sub-title="ACME 托管域名、证书到期、序列号与签发链">
      <template #extra>
        <PageActionsBar
          :meta="updatedAt ? `更新于 ${updatedAt}` : '等待采样'"
          @refresh="refresh"
        />
      </template>
    </sl-page-header>

    <CertificateOverview :stats="summaryStats" />

    <CertificateSearchBar
      :keyword="search.keyword"
      :status="search.status"
      @update:keyword="search.keyword = $event"
      @update:status="search.status = $event"
      @submit="applySearch"
      @reset="resetSearch"
    />

    <CertificateListPanel :rows="rows" />
    <CertificateDetailPanels :items="certItems" />
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { SlPageHeader } from '@sailonix/ui/page-header'
import PageActionsBar from '../../components/PageActionsBar.vue'
import CertificateDetailPanels from './components/CertificateDetailPanels.vue'
import CertificateListPanel from './components/CertificateListPanel.vue'
import CertificateOverview from './components/CertificateOverview.vue'
import CertificateSearchBar from './components/CertificateSearchBar.vue'
import { dismissNotification, notifyWarning } from '../../services/notifications'
import { apiGet } from '../../services/api'
import type { CertificatesSnapshot } from '../../types'
import type { CertificateRowView } from './types'

const snapshot = ref<CertificatesSnapshot | null>(null)
const updatedAt = ref('')
let acmeNoticeShown = false
let acmeNoticeId: number | null = null
let timer: number | undefined

const entries = computed(() => snapshot.value?.entries ?? [])
const managedCount = computed(() => entries.value.filter((entry) => entry.status === 'managed').length)
const expiringSoon = computed(
  () =>
    entries.value.filter(
      (entry) =>
        entry.days_left !== null && entry.days_left >= 0 && entry.days_left <= 30,
    ).length,
)
const expiredCount = computed(() => entries.value.filter((entry) => entry.status === 'expired').length)

const search = reactive({ keyword: '', status: 'all' })
const applied = reactive({ keyword: '', status: 'all' })

function applySearch() {
  applied.keyword = search.keyword.trim().toLowerCase()
  applied.status = search.status
}

function resetSearch() {
  search.keyword = ''
  search.status = 'all'
  applySearch()
}

type SummaryTone = 'default' | 'success' | 'danger' | 'muted'

const summaryStats = computed<
  Array<{ label: string; value: string | number; unit?: string; hint: string; tone: SummaryTone }>
>(() => [
  {
    label: '证书域名',
    value: entries.value.length,
    unit: '个',
    hint: '按 SAN 匹配到 proxy 域名',
    tone: 'default',
  },
  {
    label: '托管中',
    value: managedCount.value,
    unit: '个',
    hint: 'ACME 自动续期范围内',
    tone: 'default',
  },
  {
    label: '30 天内到期',
    value: expiringSoon.value,
    hint: expiringSoon.value > 0 ? '续期窗口已打开' : '暂无临近到期证书',
    tone: expiringSoon.value > 0 ? 'danger' : 'muted',
  },
  {
    label: '已过期',
    value: expiredCount.value,
    hint: expiredCount.value > 0 ? '需要重新签发' : '所有证书可用',
    tone: expiredCount.value > 0 ? 'danger' : 'muted',
  },
])

function statusMeta(status: string): {
  tagStatus: 'default' | 'success' | 'processing' | 'warning' | 'error'
  tagLabel: string
} {
  switch (status) {
    case 'managed':
      return { tagStatus: 'success', tagLabel: '托管中' }
    case 'expiring':
      return { tagStatus: 'warning', tagLabel: '即将到期' }
    case 'expired':
      return { tagStatus: 'error', tagLabel: '已过期' }
    case 'pending':
      return { tagStatus: 'processing', tagLabel: '等待签发' }
    default:
      return { tagStatus: 'default', tagLabel: '未配置' }
  }
}

function daysMeta(value: number | null): { text: string; tone: 'ok' | 'warn' | 'danger' } {
  if (value === null) return { text: '—', tone: 'ok' }
  if (value < 0) return { text: `已过期 ${Math.abs(value)} 天`, tone: 'danger' }
  if (value <= 30) return { text: `剩余 ${value} 天`, tone: 'warn' }
  return { text: `剩余 ${value} 天`, tone: 'ok' }
}

function formatDate(seconds: number): string {
  const date = new Date(seconds * 1000)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  return `${date.getFullYear()}-${month}-${day}`
}

function issuerText(value: string | null): string {
  if (!value) return '—'
  const cn = value.match(/CN=([^,]+)/)?.[1]?.trim() ?? value
  const org = value.match(/O=([^,]+)/)?.[1]?.trim()
  if (org?.includes("Let's Encrypt")) {
    return cn === 'R3' || cn === 'R10' || cn === 'R11' ? `Let's Encrypt ${cn}` : "Let's Encrypt"
  }
  if (org && (!cn || cn === org)) return org
  return cn
}

const allRows = computed<CertificateRowView[]>(() =>
  entries.value.map((entry) => {
    const meta = statusMeta(entry.status)
    const days = daysMeta(entry.days_left)
    return {
      ...entry,
      ...meta,
      sourceText:
        entry.source === 'acme'
          ? 'ACME'
          : entry.source === 'cache'
            ? '本地缓存'
            : entry.source === 'manual'
              ? '手动'
              : entry.source,
      challengeText: entry.challenge === 'dns-01' ? 'DNS-01' : entry.challenge === 'http-01' ? 'HTTP-01' : '—',
      errorText: entry.error ?? '',
      chainText: entry.chain_length ? `${entry.chain_length} 张` : '—',
      expiresText: entry.not_after ? formatDate(entry.not_after) : '—',
      daysText: days.text,
      daysTone: days.tone,
      serialText: entry.serial ?? '—',
      issuerText: issuerText(entry.issuer),
    }
  }),
)

const rows = computed(() =>
  allRows.value.filter((row) => {
    if (applied.status !== 'all' && row.status !== applied.status) return false
    if (!applied.keyword) return true
    return [row.domain, row.proxy, row.issuerText, row.serialText, row.type, row.sourceText]
      .join(' ')
      .toLowerCase()
      .includes(applied.keyword)
  }),
)

const certItems = computed(() => [
  { label: 'ACME', value: snapshot.value?.acme_enabled ? '已启用' : '未启用' },
  { label: '环境', value: snapshot.value?.staging ? 'staging' : '生产目录' },
  { label: '证书目录', value: snapshot.value?.cert_dir ?? '—' },
  { label: '缓存证书', value: `${snapshot.value?.cached_count ?? 0} 份` },
])

async function refresh() {
  try {
    const data = await apiGet<CertificatesSnapshot>('/api/v1/certificates')
    snapshot.value = data
    updatedAt.value = new Date().toLocaleTimeString('zh-CN', { hour12: false })
    if (!data.acme_enabled && !acmeNoticeShown) {
      acmeNoticeShown = true
      acmeNoticeId = notifyWarning(
        'ACME 当前未启用',
        '面板仍会解析 cert_dir 中的缓存证书；启用 [acme] 后，HTTP-01 证书由 rustls-acme 管理并热加载。',
      )
    } else if (data.acme_enabled) {
      acmeNoticeShown = false
      if (acmeNoticeId !== null) {
        dismissNotification(acmeNoticeId)
        acmeNoticeId = null
      }
    }
  } catch {
    // 保持上一帧。
  }
}

onMounted(() => {
  refresh()
  timer = window.setInterval(refresh, 10000)
})
onBeforeUnmount(() => {
  if (timer) window.clearInterval(timer)
  if (acmeNoticeId !== null) dismissNotification(acmeNoticeId)
})
</script>
