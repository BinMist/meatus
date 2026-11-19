<template>
  <sl-panel
    id="settings-http"
    class="settings-group"
    title="HTTP 与 ACME"
    description="HTTP / HTTPS 按 Host / SNI 路由；ACME 精确域名走 HTTP-01，泛域名走 DNS-01"
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
            description="签发成功后热加载证书，无 HTTP-01 时需 DNS-01"
            aria-label="启用 ACME"
          />
        </div>
        <sl-form-item label="ACME 邮箱" help="启用 ACME 时必填，用于证书到期通知">
          <sl-input v-model:value="draft.acme.email" placeholder="admin@example.com" />
        </sl-form-item>
        <sl-form-item label="校验方式" help="泛域名必须 DNS-01；精确域名可用 HTTP-01">
          <sl-select v-model:value="draft.acme.challenge" :options="challengeOptions" />
        </sl-form-item>
        <div class="mt-switch-cell">
          <sl-switch-card
            v-model="draft.acme.staging"
            title="Let's Encrypt staging"
            description="调试时使用，证书不被浏览器信任"
            aria-label="使用 Let's Encrypt staging"
          />
        </div>
        <sl-form-item v-if="draft.acme.challenge === 'dns-01'" label="DNS Provider" help="支持 Cloudflare 与 DNSPod；泛域名必须 DNS-01">
          <sl-select v-model:value="draft.acme.dns_provider" :options="providerOptions" />
        </sl-form-item>
        <sl-form-item
          v-if="draft.acme.challenge === 'dns-01'"
          :label="tokenLabel"
          :help="tokenHelp"
        >
          <sl-input
            v-model:value="draft.acme.dns_api_token"
            type="password"
            :placeholder="dnsTokenConfigured ? '已配置，留空保留' : tokenPlaceholder"
          />
        </sl-form-item>
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
import { SlSelect } from '@sailonix/ui/select'
import { SlSwitchCard } from '@sailonix/ui/switch'
import { computed } from 'vue'
import type { SettingsDraft, SelectOption } from '../types'

defineOptions({ name: 'SettingsHttpSection' })
const props = defineProps<{ draft: SettingsDraft; certDir: string; dnsTokenConfigured: boolean }>()

const challengeOptions: SelectOption[] = [
  { value: 'http-01', label: 'HTTP-01', description: '精确域名，需要 80 端口可达' },
  { value: 'dns-01', label: 'DNS-01', description: '泛域名，通过 DNS TXT 校验' },
]

const providerOptions: SelectOption[] = [
  { value: 'cloudflare', label: 'Cloudflare', description: '需要 Zone:Read + Zone.DNS:Edit' },
  { value: 'dnspod', label: 'DNSPod', description: '腾讯云 DNSPod，Token 格式 ID,Token' },
]

const tokenLabel = computed(() =>
  props.draft.acme.dns_provider === 'dnspod' ? 'DNSPod API Token' : 'Cloudflare API Token',
)
const tokenPlaceholder = computed(() =>
  props.draft.acme.dns_provider === 'dnspod' ? '粘贴 ID,Token' : '粘贴 API Token',
)
const tokenHelp = computed(() => {
  if (props.draft.acme.dns_provider === 'dnspod') {
    return props.dnsTokenConfigured
      ? '已配置；留空保留现有 token'
      : '腾讯云 DNSPod 的 API Token，格式为 ID,Token'
  }
  return props.dnsTokenConfigured
    ? '已配置；留空保留现有 token'
    : '需要 Zone:Read + Zone.DNS:Edit 权限'
})
</script>
