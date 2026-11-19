<template>
  <sl-panel
    id="settings-backup"
    class="settings-group"
    title="配置备份与恢复"
    description="导出脱敏备份；恢复前校验版本与校验和，失败不覆盖当前配置"
    variant="filled"
  >
    <div class="mt-backup-grid">
      <div class="mt-backup-card">
        <div class="mt-backup-title">下载备份</div>
        <p class="mt-backup-desc">备份包含配置正文、证书路径引用和版本信息；不包含 admin token、DNS token、客户端明文 token 或私钥。</p>
        <sl-button type="secondary" size="sm" :loading="downloading" @click="download">
          <template #leading><sl-icon name="download" :size="15" /></template>
          下载备份 JSON
        </sl-button>
      </div>

      <div class="mt-backup-card">
        <div class="mt-backup-title">上传恢复</div>
        <p class="mt-backup-desc">恢复使用当前面板持有的 admin token 与 DNS token 回填，恢复成功后自动热加载。</p>
        <div class="mt-backup-actions">
          <label class="mt-backup-file">
            <input type="file" accept="application/json,.json" @change="onFile" />
            <span>{{ fileName || '选择备份 JSON' }}</span>
          </label>
          <sl-button
            type="primary"
            size="sm"
            :disabled="!backup"
            :loading="restoring"
            @click="restore"
          >
            恢复配置
          </sl-button>
        </div>
        <div v-if="backup" class="mt-backup-meta">
          <span>{{ backup.kind }} · {{ backup.format }}</span>
          <span>版本 {{ backup.meatus_version }}</span>
          <span>SHA256 {{ backup.config_sha256.slice(0, 12) }}…</span>
        </div>
        <div v-if="restoredPath" class="mt-backup-result">
          已恢复；原配置备份在 <code>{{ restoredPath }}</code>
        </div>
        <ul v-if="warnings.length" class="mt-backup-warnings">
          <li v-for="warning in warnings" :key="warning">{{ warning }}</li>
        </ul>
      </div>
    </div>
  </sl-panel>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { SlButton } from '@sailonix/ui/button'
import { SlIcon } from '@sailonix/icon'
import { SlPanel } from '@sailonix/ui/panel'
import { apiGet, apiWrite } from '../../../services/api'
import { notifyError, notifySuccess, notifyWarning } from '../../../services/notifications'

defineOptions({ name: 'SettingsBackupSection' })
const emit = defineEmits<{ restored: [] }>()

interface BackupDocument {
  schema_version: number
  meatus_version: string
  created_at_ms: number
  kind: string
  format: string
  config: string
  config_sha256: string
  certificates: Array<{ role: string; path: string }>
  checksum: string
}

interface BackupResponse {
  filename: string
  backup: BackupDocument
}

interface RestoreResponse {
  ok: boolean
  previous: string | null
  warnings: string[]
  config_version: string
}

const downloading = ref(false)
const restoring = ref(false)
const fileName = ref('')
const backup = ref<BackupDocument | null>(null)
const restoredPath = ref('')
const warnings = ref<string[]>([])

async function download() {
  downloading.value = true
  try {
    const data = await apiGet<BackupResponse>('/api/v1/config/backup')
    const blob = new Blob([JSON.stringify(data.backup, null, 2)], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = data.filename
    anchor.click()
    URL.revokeObjectURL(url)
    notifySuccess('备份已导出', data.filename)
  } catch (error) {
    notifyError('导出失败', error instanceof Error ? error.message : String(error))
  } finally {
    downloading.value = false
  }
}

async function onFile(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file) return
  fileName.value = file.name
  restoredPath.value = ''
  warnings.value = []
  try {
    const parsed = JSON.parse(await file.text()) as BackupDocument
    if (!parsed.schema_version || !parsed.config || !parsed.checksum) {
      throw new Error('不是有效的 Meatus 备份文件')
    }
    backup.value = parsed
  } catch (error) {
    backup.value = null
    notifyError('备份文件无效', error instanceof Error ? error.message : String(error))
  }
}

async function restore() {
  if (!backup.value) return
  if (!window.confirm('恢复会覆盖当前 server.toml，并立即热加载；确认继续？')) return
  restoring.value = true
  try {
    const result = await apiWrite<RestoreResponse>('POST', '/api/v1/config/restore', backup.value)
    restoredPath.value = result.previous ?? ''
    warnings.value = result.warnings ?? []
    if (warnings.value.length) {
      notifyWarning('配置已恢复，但有注意事项', warnings.value.join('；'))
    } else {
      notifySuccess('配置已恢复', `版本 ${result.config_version}`)
    }
    backup.value = null
    fileName.value = ''
    emit('restored')
  } catch (error) {
    notifyError('恢复失败，当前配置未覆盖', error instanceof Error ? error.message : String(error))
  } finally {
    restoring.value = false
  }
}
</script>
