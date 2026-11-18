<template>
  <div class="page">
    <sl-page-header title="系统设置" sub-title="在线修改 server.toml；保存后面板会短暂中断并自动重启">
      <template #extra>
        <PageActionsBar meta="在线配置 · server.toml" @refresh="refresh">
          <template #prefix>
            <sl-status-tag v-if="dirty" class="mode-chip" status="warning" size="sm" label="未保存修改" />
          </template>
          <sl-button
            type="primary"
            size="sm"
            :disabled="!canSave || !dirty"
            @click="openConfirm"
          >
            保存并重启
          </sl-button>
        </PageActionsBar>
      </template>
    </sl-page-header>

    <div class="settings">
      <SettingsNav :items="anchorItems" :active-key="activeSection" @select="onAnchorSelect" />

      <div class="settings-body">
        <SettingsControlSection :draft="draft" />
        <SettingsHttpSection :draft="draft" :cert-dir="settings?.acme.cert_dir || ''" />
        <SettingsSocksSection
          :draft="draft"
          :log-level-options="logLevelOptions"
          :log-format-options="logFormatOptions"
        />
        <SettingsReadonlySection :items="readonlyItems" />
        <SettingsBehaviorNotes />
        <SettingsFoot :text="footText" />
      </div>
    </div>

    <SettingsSaveDialog
      v-model:open="confirmOpen"
      :saving="saving"
      :restarting="restarting"
      @confirm="save"
      @cancel="confirmOpen = false"
    />
  </div>
</template>

<script setup lang="ts">
import './styles.scss'
import { computed, onBeforeUnmount, onMounted, reactive, ref } from 'vue'
import { SlButton } from '@sailonix/ui/button'
import { SlPageHeader } from '@sailonix/ui/page-header'
import { SlStatusTag } from '@sailonix/ui/status-tag'
import PageActionsBar from '../../components/PageActionsBar.vue'
import SettingsBehaviorNotes from './components/SettingsBehaviorNotes.vue'
import SettingsControlSection from './components/SettingsControlSection.vue'
import SettingsFoot from './components/SettingsFoot.vue'
import SettingsHttpSection from './components/SettingsHttpSection.vue'
import SettingsNav from './components/SettingsNav.vue'
import SettingsReadonlySection from './components/SettingsReadonlySection.vue'
import SettingsSaveDialog from './components/SettingsSaveDialog.vue'
import SettingsSocksSection from './components/SettingsSocksSection.vue'
import { dismissNotification, notifyError, notifySuccess, notifyWarning } from '../../services/notifications'
import { apiGet, apiWrite } from '../../services/api'
import type { SettingsSnapshot, SettingsUpdateResponse } from '../../types'
import type { SelectOption, SettingsDraft, SettingsNavItem } from './types'

const settings = ref<SettingsSnapshot | null>(null)
const lastRead = ref('')
let editNoticeShown = false
let editNoticeId: number | null = null
const saving = ref(false)
const restarting = ref(false)
const confirmOpen = ref(false)
let alive = true

const sections = [
  { id: 'settings-control', label: '控制面与端口' },
  { id: 'settings-http', label: 'HTTP 与 ACME' },
  { id: 'settings-socks', label: 'SOCKS5 与日志' },
  { id: 'settings-readonly', label: '只读运行信息' },
]

const anchorItems: SettingsNavItem[] = sections.map((section) => ({
  key: section.id,
  label: section.label,
  href: `#${section.id}`,
  title: section.label,
}))

const activeSection = ref(sections[0]?.id ?? '')

function onAnchorSelect(key: string) {
  activeSection.value = key || activeSection.value
}

const draft = reactive<SettingsDraft>({
  server: { bind: '', control_port: 7000 },
  tenant: { default: 'default' },
  ports: { start: 20000, end: 30000 },
  http: { enabled: false, bind: '0.0.0.0:80', https_bind: '0.0.0.0:443', domain: '' },
  acme: { enabled: false, email: '', staging: false },
  socks5: { enabled: false, bind: '0.0.0.0:2081' },
  log: { level: 'info', format: 'text' },
})

const logLevelOptions: SelectOption[] = [
  { value: 'trace', label: 'trace', description: '最详细，仅排障时使用' },
  { value: 'debug', label: 'debug', description: '调试信息' },
  { value: 'info', label: 'info', description: '默认，生产建议' },
  { value: 'warn', label: 'warn', description: '只保留警告与错误' },
  { value: 'error', label: 'error', description: '只保留错误' },
]

const logFormatOptions: SelectOption[] = [
  { value: 'text', label: 'text', description: '人类可读文本' },
  { value: 'json', label: 'json', description: '结构化日志，适合采集' },
]

const canEdit = computed(() => Boolean(settings.value?.writes.settings))
const canSave = computed(
  () => canEdit.value && !saving.value && !restarting.value && Boolean(settings.value),
)
const dirty = computed(() => {
  if (!settings.value) return false
  return JSON.stringify(toDraft(settings.value)) !== JSON.stringify(draft)
})

const readonlyItems = computed(() => {
  const current = settings.value
  if (!current) return []
  return [
    { label: 'server 绑定', value: `${current.server.bind}:${current.server.control_port}` },
    { label: 'TLS 证书', value: current.server.tls_cert || '未配置（自签或透传）' },
    { label: 'admin 绑定', value: current.admin.bind },
    { label: 'admin_token', value: current.admin.token_configured ? '已配置' : '未配置（仅 loopback）' },
    { label: '静态面板', value: current.admin.panel ? '已启用' : '未启用' },
    { label: '仅 loopback', value: current.admin.loopback ? '是' : '否' },
    { label: 'ACME 校验', value: current.acme.challenge },
    { label: '会话有效期', value: formatDuration(current.session.ttl_s) },
    { label: 'CSRF', value: current.session.csrf_required ? '写操作强制校验' : '未启用' },
    { label: '登录失败锁定', value: `${current.session.login_max_failures} 次 / ${formatDuration(current.session.login_lock_s)}` },
    { label: '配置写回', value: current.writes.settings ? '支持' : '不支持' },
  ]
})

const footText = computed(() =>
  settings.value
    ? `配置路径：${settings.value.writes.config_path || '未知'} · 最后读取：${lastRead.value}`
    : '配置读取中…',
)

function toDraft(current: SettingsSnapshot): SettingsDraft {
  return {
    server: { bind: current.server.bind, control_port: current.server.control_port },
    tenant: { default: current.tenant.default },
    ports: { start: current.ports.range[0], end: current.ports.range[1] },
    http: {
      enabled: current.http.enabled,
      bind: current.http.bind,
      https_bind: current.http.https_bind,
      domain: current.http.domain,
    },
    acme: {
      enabled: current.acme.enabled,
      email: current.acme.email ?? '',
      staging: current.acme.staging,
    },
    socks5: { enabled: current.socks5.enabled, bind: current.socks5.bind },
    log: { level: current.log.level, format: current.log.format },
  }
}

function applySnapshot(current: SettingsSnapshot) {
  const next = toDraft(current)
  Object.assign(draft.server, next.server)
  Object.assign(draft.tenant, next.tenant)
  Object.assign(draft.ports, next.ports)
  Object.assign(draft.http, next.http)
  Object.assign(draft.acme, next.acme)
  Object.assign(draft.socks5, next.socks5)
  Object.assign(draft.log, next.log)
}

function formatDuration(seconds: number): string {
  if (!seconds) return '—'
  if (seconds >= 3600) return `${Math.round(seconds / 3600)} 小时`
  if (seconds >= 60) return `${Math.round(seconds / 60)} 分钟`
  return `${seconds} 秒`
}

function validateDraft(): string {
  if (draft.server.control_port < 1 || draft.server.control_port > 65535) return '控制端口必须在 1–65535 之间'
  if (draft.ports.start < 1 || draft.ports.start > 65535) return '端口池起始端口无效'
  if (draft.ports.end < 1 || draft.ports.end > 65535) return '端口池结束端口无效'
  if (draft.ports.start > draft.ports.end) return '端口池起始端口不能大于结束端口'
  if (draft.http.enabled && !draft.http.domain.trim()) return '启用 HTTP 时需要填写默认域名'
  if (draft.acme.enabled && !draft.acme.email.trim()) return '启用 ACME 时需要填写邮箱'
  if (draft.acme.enabled && !draft.http.enabled) return '启用 ACME 时需要同时启用 HTTP（HTTP-01 challenge）'
  return ''
}

function openConfirm() {
  const validation = validateDraft()
  if (validation) {
    notifyError('无法保存', validation)
    return
  }
  confirmOpen.value = true
}

async function save() {
  const validation = validateDraft()
  if (validation) {
    notifyError('无法保存', validation)
    return
  }
  saving.value = true
  try {
    const body = {
      server: { bind: draft.server.bind, control_port: draft.server.control_port },
      tenant: { default: draft.tenant.default },
      ports: { range: [draft.ports.start, draft.ports.end] },
      http: {
        enabled: draft.http.enabled,
        bind: draft.http.bind,
        https_bind: draft.http.https_bind,
        domain: draft.http.domain.trim(),
      },
      acme: {
        enabled: draft.acme.enabled,
        email: draft.acme.email.trim(),
        staging: draft.acme.staging,
      },
      socks5: { enabled: draft.socks5.enabled, bind: draft.socks5.bind },
      log: { level: draft.log.level, format: draft.log.format },
    }
    const result = await apiWrite<SettingsUpdateResponse>('PUT', '/api/v1/settings', body)
    confirmOpen.value = false
    notifySuccess('配置已保存', result.backup ? `${result.message} 备份：${result.backup}` : result.message)
    restarting.value = true
    void waitForRestart()
  } catch (err) {
    notifyError('保存失败', err instanceof Error ? err.message : '保存失败')
  } finally {
    saving.value = false
  }
}

async function waitForRestart() {
  const deadline = Date.now() + 30_000
  while (alive && Date.now() < deadline) {
    await sleep(1000)
    try {
      const response = await fetch('/healthz', { cache: 'no-store' })
      if (response.ok) {
        window.location.reload()
        return
      }
    } catch {
      // 重启窗口内连接失败是预期的，继续等。
    }
  }
  if (alive) {
    restarting.value = false
    notifyError('服务重启超时', '请手动刷新页面，或检查容器 / systemd 的运行状态。')
  }
}

function sleep(ms: number) {
  return new Promise((resolve) => window.setTimeout(resolve, ms))
}

async function refresh() {
  try {
    const data = await apiGet<SettingsSnapshot>('/api/v1/settings')
    settings.value = data
    applySnapshot(data)
    lastRead.value = new Date().toLocaleTimeString('zh-CN', { hour12: false })
    if (!data.writes.settings && !editNoticeShown) {
      editNoticeShown = true
      editNoticeId = notifyWarning(
        '当前实例不支持在线修改',
        'server 启动时没有配置文件路径，请用 meatus server --config /path/to/server.toml 启动后重试。',
      )
    } else if (data.writes.settings) {
      editNoticeShown = false
      if (editNoticeId !== null) {
        dismissNotification(editNoticeId)
        editNoticeId = null
      }
    }
  } catch (err) {
    notifyError('配置读取失败', err instanceof Error ? err.message : '读取失败')
  }
}

onMounted(refresh)
onBeforeUnmount(() => {
  alive = false
  if (editNoticeId !== null) dismissNotification(editNoticeId)
})
</script>
