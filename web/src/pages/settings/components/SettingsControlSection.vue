<template>
  <sl-panel
    id="settings-control"
    class="settings-group"
    title="控制面与端口"
    description="server 监听地址、默认租户与 TCP / UDP 端口池"
  >
    <sl-form :model="draft" label-position="top" @submit.prevent>
      <div class="mt-form-grid mt-form-grid--four">
        <sl-form-item label="控制面监听地址" help="例如 0.0.0.0 或 127.0.0.1">
          <sl-input v-model:value="draft.server.bind" placeholder="0.0.0.0" />
        </sl-form-item>
        <sl-form-item label="控制端口" help="客户端连接端口，修改后需要客户端同步">
          <sl-number-field
            v-model:value="draft.server.control_port"
            :min="1"
            :max="65535"
            :format-options="{ useGrouping: false }"
          />
        </sl-form-item>
        <sl-form-item label="默认租户" help="未显式指定 tenant 时使用">
          <sl-input v-model:value="draft.tenant.default" placeholder="default" />
        </sl-form-item>
        <sl-form-item label="端口池范围" help="server 为 TCP / UDP proxy 分配的远端端口区间">
          <div class="mt-port-range">
            <sl-number-field
              v-model:value="draft.ports.start"
              :min="1"
              :max="65535"
              :format-options="{ useGrouping: false }"
            />
            <span class="mt-port-range__sep">至</span>
            <sl-number-field
              v-model:value="draft.ports.end"
              :min="1"
              :max="65535"
              :format-options="{ useGrouping: false }"
            />
          </div>
        </sl-form-item>
        <sl-form-item label="客户端离线端口释放" help="立即释放，或保留一段时间让原客户端重连取回">
          <sl-select v-model:value="draft.ports.release_policy" :options="releaseOptions" />
        </sl-form-item>
        <sl-form-item
          label="离线保留时长（秒）"
          help="hold 时保留端口；immediate 时仅保留 HTTP 503 标记；0 表示关闭标记"
        >
          <sl-number-field
            v-model:value="draft.ports.hold_secs"
            :min="0"
            :max="86400"
            :format-options="{ useGrouping: false }"
          />
        </sl-form-item>
      </div>
    </sl-form>
  </sl-panel>
</template>

<script setup lang="ts">
import { SlForm, SlFormItem } from '@sailonix/ui/form'
import { SlInput } from '@sailonix/ui/input'
import { SlNumberField } from '@sailonix/ui/number-field'
import { SlPanel } from '@sailonix/ui/panel'
import { SlSelect } from '@sailonix/ui/select'
import type { SettingsDraft } from '../types'

defineOptions({ name: 'SettingsControlSection' })
defineProps<{ draft: SettingsDraft }>()

const releaseOptions = [
  { value: 'immediate', label: '立即释放', description: '客户端断开后立刻归还公网端口' },
  { value: 'hold', label: '保留一段时间', description: '保留端口，原客户端重连时优先取回' },
]
</script>
