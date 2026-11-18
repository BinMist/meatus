<template>
  <sl-panel
    id="settings-socks"
    class="settings-group"
    title="SOCKS5 与日志"
    description="内置 SOCKS5 复用 server 监听；日志格式影响采集链路"
  >
    <sl-form :model="draft" label-position="top" @submit.prevent>
      <div class="mt-form-grid mt-form-grid--four">
        <div class="mt-switch-cell">
          <sl-switch-card
            v-model="draft.socks5.enabled"
            title="启用 SOCKS5"
            description="为客户端提供一个共享出口，不单独分配端口"
            aria-label="启用 SOCKS5"
          />
        </div>
        <sl-form-item label="SOCKS5 监听" help="客户端共享出口，不单独分配端口">
          <sl-input v-model:value="draft.socks5.bind" placeholder="0.0.0.0:2081" />
        </sl-form-item>
        <sl-form-item label="日志级别" help="影响写入磁盘的日志量">
          <sl-select v-model:value="draft.log.level" :options="logLevelOptions" />
        </sl-form-item>
        <sl-form-item label="日志格式" help="text 可读，json 适合采集">
          <sl-select v-model:value="draft.log.format" :options="logFormatOptions" />
        </sl-form-item>
      </div>
    </sl-form>
  </sl-panel>
</template>

<script setup lang="ts">
import { SlForm, SlFormItem } from '@sailonix/ui/form'
import { SlInput } from '@sailonix/ui/input'
import { SlPanel } from '@sailonix/ui/panel'
import { SlSelect } from '@sailonix/ui/select'
import { SlSwitchCard } from '@sailonix/ui/switch'
import type { SelectOption, SettingsDraft } from '../types'

defineOptions({ name: 'SettingsSocksSection' })
defineProps<{ draft: SettingsDraft; logLevelOptions: SelectOption[]; logFormatOptions: SelectOption[] }>()
</script>
