<template>
  <sl-panel
    id="settings-http"
    class="settings-group"
    title="HTTP 与 ACME"
    description="HTTP / HTTPS 按 Host / SNI 路由；ACME 使用 HTTP-01 自动签发"
  >
    <sl-form :model="draft" label-position="top" @submit.prevent>
      <div class="mt-form-grid mt-form-grid--four">
        <div class="mt-switch-cell">
          <sl-switch-card
            v-model="draft.http.enabled"
            title="启用 HTTP 路由"
            description="在 HTTP 监听端口按 Host 匹配 proxy 域名"
            aria-label="启用 HTTP 路由"
          />
        </div>
        <sl-form-item label="默认域名" help="HTTP / HTTPS 未匹配自定义域名时回落到这里">
          <sl-input v-model:value="draft.http.domain" placeholder="example.com" />
        </sl-form-item>
        <sl-form-item label="HTTP 监听" help="明文入口；未匹配域名时回落">
          <sl-input v-model:value="draft.http.bind" placeholder="0.0.0.0:80" />
        </sl-form-item>
        <sl-form-item label="HTTPS 监听" help="TLS 入口；证书由 ACME 或 cert_dir 提供">
          <sl-input v-model:value="draft.http.https_bind" placeholder="0.0.0.0:443" />
        </sl-form-item>
        <div class="mt-switch-cell">
          <sl-switch-card
            v-model="draft.acme.enabled"
            title="启用 ACME"
            description="HTTP-01 挑战通过后自动签发并热加载证书"
            aria-label="启用 ACME"
          />
        </div>
        <sl-form-item label="ACME 邮箱" help="启用 ACME 时必填，用于证书到期通知">
          <sl-input v-model:value="draft.acme.email" placeholder="admin@example.com" />
        </sl-form-item>
        <div class="mt-switch-cell">
          <sl-switch-card
            v-model="draft.acme.staging"
            title="Let's Encrypt staging"
            description="调试时使用，证书不被浏览器信任"
            aria-label="使用 Let's Encrypt staging"
          />
        </div>
        <sl-form-item label="证书目录（只读）" help="ACME 证书与账号缓存写入此目录">
          <sl-input :model-value="certDir" placeholder="未配置" readonly />
        </sl-form-item>
      </div>
    </sl-form>
  </sl-panel>
</template>

<script setup lang="ts">
import { SlForm, SlFormItem } from '@sailonix/ui/form'
import { SlInput } from '@sailonix/ui/input'
import { SlPanel } from '@sailonix/ui/panel'
import { SlSwitchCard } from '@sailonix/ui/switch'
import type { SettingsDraft } from '../types'

defineOptions({ name: 'SettingsHttpSection' })
defineProps<{ draft: SettingsDraft; certDir: string }>()
</script>
